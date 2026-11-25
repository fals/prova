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

/// When the process tree last used CPU, if it was busy over the window:
/// `samples` are (sample time, CPU ticks since the previous sample) for the
/// last `BUSY_WINDOW`. Busy is `BUSY_TICKS` or more in all; the time is that
/// of the newest sample with any ticks.
fn busy_since(samples: &VecDeque<(Instant, u64)>) -> Option<Instant> {
    if samples.iter().map(|(_, d)| d).sum::<u64>() < BUSY_TICKS {
        return None;
    }
    samples.iter().rev().find(|(_, d)| *d > 0).map(|(t, _)| *t)
}
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
    /// Called with the start of an output line not finished yet (a test
    /// runner prints a test's name before its outcome).
    fn on_partial(&mut self, _text: &str, _now_ms: u64) -> Vec<EventKind> {
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
    Line(usize, String, Arrived),
    /// The start of a line not finished yet (pytest's progress dots before
    /// a hang): shown in the live output at once (round 8).
    Partial(usize, String, Arrived),
    Eof,
}

/// When a chunk's bytes arrived: on the monotonic clock (timers) and on
/// the boot clock process start times use (Linux). The boot clock is read
/// here, not derived from the monotonic one: a paused VM advances one and
/// not the other (round 12, arm64 CI: the runner's own bash looked started
/// inside a test).
#[derive(Clone, Copy)]
struct Arrived {
    at: Instant,
    boot_ms: Option<u64>,
}

impl Arrived {
    fn now() -> Arrived {
        Arrived {
            at: Instant::now(),
            boot_ms: crate::process::uptime_ms(),
        }
    }
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
                        if tx.send(Chunk::Line(id, line, Arrived::now())).is_err() {
                            return;
                        }
                    }
                    if !pending.is_empty() {
                        let part = String::from_utf8_lossy(&pending).into_owned();
                        if tx.send(Chunk::Partial(id, part, Arrived::now())).is_err() {
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
                Arrived::now(),
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
    /// The framework's own count of failures, when it printed one.
    framework_failed: Option<u64>,
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
    line_at: Option<Arrived>,
    /// When the run's first output line arrived, in ms since boot.
    first_line_ms: Option<u64>,
    /// The container client the tests run through, if any.
    container: Option<&'static str>,
    /// The client's command line, to find its container when the run is
    /// stopped (round 15).
    container_cmd: Option<String>,
    /// The note that the tests run through a container client was emitted
    /// (once, as soon as the client is known: a wait reads it to give a
    /// stall the time it needs, ADR 0031 §4).
    container_noted: bool,
    /// The run's working directory and start (ms since the epoch): a
    /// container started without a name is found by what it mounts and when
    /// it was created (round 16).
    run_cwd: String,
    run_started_ms: u64,
    /// Lifecycle keys of results re-keyed under another name (a Go test
    /// once its package is known): their outcome is the new key's.
    superseded: std::collections::BTreeSet<String>,
    /// The run's test phase, from its first test event (ADR 0026 §2).
    phase: Option<Phase>,
    /// From the first output line to the end: the phase of a run whose
    /// tests are known only from result files at the end (round 13: Java
    /// and C# stayed "unknown" after 25 runs).
    whole: Option<Phase>,
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

/// One sample of a run phase: what it used beyond the runner (ADR 0026).
fn sample_phase(
    p: &mut Phase,
    now: &crate::resources::Usage,
    now_boot: Option<u64>,
    first_line_ms: Option<u64>,
) {
    let mut s = crate::resources::seen_new(&p.base_pids, &p.base_sockets, now);
    if let (Some(from), Some(to), Some(since)) = (p.start_ms, now_boot, first_line_ms) {
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

/// The least idle time a run through a container client is called stalled
/// after: its tests run out of sight of the process tree.
const CONTAINER_STALL_AFTER: Duration = Duration::from_secs(60);

/// How far from a test's start and finish lines a sample must be to count
/// for that test (ADR 0025 §5: a missed resource is a lower bound, a
/// misattributed one is wrong).
const SIZE_GUARD: Duration = Duration::from_millis(50);

impl Supervisor<'_> {
    fn emit(&mut self, mut kind: EventKind) {
        if let EventKind::RunTotals { tests, failed } = &kind {
            self.framework_total = Some(*tests);
            self.framework_failed = Some(*failed);
        }
        // The run's test phase starts at its first test event (ADR 0026).
        if matches!(
            kind,
            EventKind::TestStarted { .. } | EventKind::TestFinished { .. }
        ) && self.pgid != 0
        {
            let arrived = self.line_at.unwrap_or_else(Arrived::now);
            let at = arrived.at;
            if self.phase.is_none() {
                let _ = self.usage_now();
                self.phase = Some(Phase {
                    start_ms: arrived.boot_ms,
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
                        started: self.line_at.map_or_else(Instant::now, |a| a.at),
                        start_ms: self.line_at.unwrap_or_else(Arrived::now).boot_ms,
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
                let read = self.line_at.map_or_else(Instant::now, |a| a.at);
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
        if let EventKind::TestSuperseded { test, by } = &kind {
            self.superseded.insert(test.unique());
            // One test, one result: the provisional entry leaves the
            // verdict once its test's own result is recorded (spec/run.qnt
            // `superseded`; round 15: a rerun's pass did not count).
            let _ = self.lifecycle.supersede(&test.unique(), &by.unique());
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
                // A test executed again after a pass and failing (Go's
                // `-count=N`) is a later attempt: the failure is recorded.
                let mut ok = self
                    .lifecycle
                    .test_finished(&test.unique(), *outcome)
                    .is_ok();
                if !ok && *attempt > 1 {
                    ok = self
                        .lifecycle
                        .test_repeated(&test.unique(), *outcome)
                        .is_ok();
                }
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
            && self.container.is_none()
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
        // default in `make` while `default` knew it takes 3 ms). Only the
        // p95 is needed, and this runs once per test while recording: the
        // full `stats` made a run with history cost milliseconds per test
        // (issue 91).
        match crate::analysis::usual_p95_ms(self.store, &id, &test.display()) {
            Ok(Some(p95)) => (
                Duration::from_millis((p95 * 3).max(1000)),
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

    /// The run's test phase as one observation: samples taken before its
    /// last test event, at least two of them, or nothing (ADR 0026 §2).
    fn phase_seen(&self) -> Option<crate::resources::Seen> {
        let from = |p: &Phase, guard: bool| {
            let mut seen = crate::resources::Seen::default();
            for (at, s) in &p.samples {
                if !guard || p.last_event.saturating_duration_since(*at) >= SIZE_GUARD {
                    seen.absorb(s);
                }
            }
            (seen.samples >= 2).then_some(seen)
        };
        match &self.phase {
            Some(p) if p.samples.len() >= 2 => from(p, true),
            // No test event came while the command ran: the whole run.
            _ => self.whole.as_ref().and_then(|w| from(w, false)),
        }
    }

    /// Starts the whole-run phase at the first output line.
    fn start_whole(&mut self, at: Arrived) {
        if self.pgid == 0 || self.whole.is_some() {
            return;
        }
        let _ = self.usage_now();
        self.whole = Some(Phase {
            start_ms: at.boot_ms,
            base_pids: self.known_pids.clone(),
            base_sockets: self.known_sockets.clone(),
            last_event: at.at,
            samples: Vec::new(),
        });
    }

    /// Says once that the tests run through a container client.
    fn note_container(&mut self) {
        if let Some(client) = self.container
            && !self.container_noted
        {
            self.container_noted = true;
            self.emit(EventKind::RunNote {
                message: format!(
                    "the tests ran through `{client}`: their processes and sockets are out of prova's sight, so no size is recorded for them"
                ),
            });
        }
    }

    /// The run is being stopped: stopping a container client does not stop
    /// its container. A container the run named (`--name`, `--cidfile`) is
    /// killed; any other is not attributable, so the run says to check.
    fn stop_container(&mut self) {
        let Some(client) = self.container else {
            return;
        };
        let cmd = self.container_cmd.clone().unwrap_or_default();
        match crate::resources::container_of(&cmd) {
            Some(target) if crate::resources::container_client_runs(&cmd) => {
                let ok = crate::resources::kill_container(client, &target);
                let message = if ok {
                    format!("stopped the container `{target}` with `{client} kill {target}`: stopping the `{client}` client alone does not stop it")
                } else {
                    format!("could not stop the container `{target}` with `{client} kill {target}`: check `{client} ps` and stop it")
                };
                if ok {
                    self.emit(EventKind::RunNote { message });
                } else {
                    self.emit(EventKind::RunWarning { message });
                }
            }
            _ if crate::resources::container_client_runs(&cmd) => {
                let found = crate::resources::containers_of_run(
                    client,
                    &cmd,
                    &self.run_cwd,
                    self.run_started_ms,
                );
                match found.as_slice() {
                    [one] => {
                        let ok = crate::resources::kill_container(client, one);
                        let message = if ok {
                            format!("stopped the container `{one}` with `{client} kill {one}` (it mounts the run's directory and was created after the run started; the `{client}` command had no --name or --cidfile)")
                        } else {
                            format!("could not stop the container `{one}`: run `{client} kill {one}`")
                        };
                        if ok {
                            self.emit(EventKind::RunNote { message });
                        } else {
                            self.emit(EventKind::RunWarning { message });
                        }
                    }
                    [] => self.emit(EventKind::RunWarning {
                        message: format!(
                            "the tests ran through `{client}`: stopping its client does not stop its container, and none could be told from the run (no --name or --cidfile; no running container mounts this directory); check `{client} ps` and stop it"
                        ),
                    }),
                    many => self.emit(EventKind::RunWarning {
                        message: format!(
                            "the tests ran through `{client}`: stopping its client does not stop its container, and {} running containers could be it; stop the right one: {}",
                            many.len(),
                            many.iter()
                                .map(|id| format!("`{client} kill {id}`"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    }),
                }
            }
            _ => self.emit(EventKind::RunWarning {
                message: format!(
                    "the tests ran through `{client}`: stopping its client does not stop its container; check `{client} ps` and stop it"
                ),
            }),
        }
    }

    /// A snapshot of the command's group, reused when under 50 ms old.
    fn usage_now(&mut self) -> crate::resources::Usage {
        if let Some((at, u)) = &self.last_usage
            && at.elapsed() < Duration::from_millis(20)
        {
            return u.clone();
        }
        let u = crate::resources::sample(self.pgid);
        // A container client started before the tests printed anything is
        // how the command runs them: what they use is out of sight.
        if self.container.is_none() {
            let found = u.procs.iter().find_map(|(pid, (_, cmd))| {
                let early = match (u.starts.get(pid), self.first_line_ms) {
                    (Some(start), Some(first)) => *start <= first,
                    _ => true,
                };
                crate::resources::container_client(cmd)
                    .filter(|_| early)
                    .map(|c| (c, cmd.clone()))
            });
            if let Some((client, cmd)) = found {
                self.container = Some(client);
                self.container_cmd = Some(cmd);
                self.note_container();
            }
        }
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
        let now_boot = crate::process::uptime_ms();
        // The run's test phase: what started inside it, beyond the runner.
        let first_line_ms = self.first_line_ms;
        for p in [self.phase.as_mut(), self.whole.as_mut()]
            .into_iter()
            .flatten()
        {
            sample_phase(p, now, now_boot, first_line_ms);
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
    fn partial(&mut self, stream: usize, text: &str, observer: &mut dyn Observer) {
        let done = &mut self.partial[stream];
        let new = text.strip_prefix(done.as_str()).unwrap_or(text);
        let _ = self.output.write_all(new.as_bytes());
        let _ = self.output.flush();
        *done = text.to_owned();
        let events = observer.on_partial(text, now_ms());
        self.observed(events, observer);
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
            .unwrap_or(crate::config::DEFAULT_SLOW),
        running: BTreeMap::new(),
        terminating: false,
        partial: Default::default(),
        framework_total: None,
        framework_failed: None,
        cpu_start: None,
        pgid: 0,
        sizes: BTreeMap::new(),
        last_usage: None,
        known_pids: Default::default(),
        known_sockets: Default::default(),
        line_at: None,
        first_line_ms: None,
        // The command itself may be the client (`docker run ... go test`):
        // known before any sample, however short the run (round 14 CI).
        container: crate::resources::container_client(&spec.command.display()),
        container_cmd: Some(spec.command.display()),
        container_noted: false,
        run_cwd: row.cwd.clone(),
        run_started_ms: started_ms,
        superseded: Default::default(),
        phase: None,
        whole: None,
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
    let framework_filter = spec
        .selection
        .as_deref()
        .map(|f| crate::config::framework_filter(&spec.command.display(), f))
        .unwrap_or_default();
    for (k, v) in &injected {
        cmd.env(k, v);
    }
    cmd.current_dir(&row.cwd)
        .env("PROVA_RUN_ID", run)
        // The selection reaches the shell only as a variable, never as text.
        .env("PROVA_FILTER", &framework_filter)
        // The same, single-quoted, for a filter inside a wrapper's quoted
        // argument (round 17).
        .env(
            "PROVA_FILTER_Q",
            crate::config::shell_single_quote(&framework_filter),
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
    sup.note_container();
    // A parameterized case's selection that the framework can only take as
    // its whole method: all its cases run (round 15: no word of it).
    if let Some(sel) = spec.selection.as_deref() {
        let filter = crate::config::framework_filter(&spec.command.display(), sel);
        if (sel.ends_with(']') || sel.ends_with(')')) && sel.contains('(') && !filter.contains('(')
        {
            sup.emit(EventKind::RunNote {
                message: format!(
                    "the framework selects {filter:?}, not one of its cases: every case of it runs"
                ),
            });
        }
    }
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
            // From prova.toml or `--inject` alike (round 14: the note
            // named prova.toml for a recipe given on the command line).
            message: format!(
                "injected by opt-in recipe {}: {}",
                spec.inject.join(", "),
                vars.join("; ")
            ),
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

    let declared_stall = Duration::from_millis(spec.stall_after_ms);
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
                        sup.first_line_ms = at.boot_ms;
                        sup.start_whole(at);
                    }
                    sup.line(i, &l, observer);
                    sup.line_at = None;
                }
                Ok(Chunk::Partial(i, p, _)) => {
                    last_activity = Instant::now();
                    sup.partial(i, &p, observer);
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
            // The window decides whether the tree is busy at all; the time
            // credited is that of the latest sample that used CPU, not
            // `now`: CPU used 4 s ago is not activity now, and crediting it
            // as such delayed a stall by the whole window (issue #89).
            if let Some(at) = busy_since(&recent_ticks) {
                last_activity = last_activity.max(at);
            }
            if !s.pids.is_subset(&sample.pids) {
                last_activity = now;
            }
            sample = s;
            if terminating.is_none() && store.cancel_requested(run)? {
                process::terminate_group(pgid);
                sup.terminating = true;
                sup.stop_container();
                terminating = Some((now, Status::Cancelled, "cancelled".into()));
            }
        }
        let idle = now.duration_since(last_activity);
        // Through a container client the host process tree is idle by
        // design (round 16: a NuGet restore in docker was called a stall at
        // 20 s): process inactivity alone is no stall before a minute.
        let stall_after = if sup.container.is_some() {
            declared_stall.max(CONTAINER_STALL_AFTER)
        } else {
            declared_stall
        };
        if idle >= stall_after && !stalled {
            stalled = true;
            if let Some(client) = sup.container {
                sup.emit(EventKind::RunNote {
                    message: format!(
                        "the tests run in a container (`{client}`): prova sees only its output, not what runs inside, so a stall is called after at least {}",
                        crate::ids::format_duration(CONTAINER_STALL_AFTER)
                    ),
                });
            }
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
            sup.stop_container();
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

    // The command's wall time is what the run lasted, however long the
    // recording that follows takes (issue #91).
    let exited_ms = now_ms();
    // Collect remaining output; leaked processes may hold the pipes open,
    // so never wait on EOF without a bound. The bound is on waiting for
    // output, not on recording it: every line that arrived before the
    // deadline is recorded however long that takes (round 12: with history,
    // recording 723 Go results took over 1 s and two thirds were dropped
    // from a run that still passed).
    //
    // Recording can lag far behind the command (issue #91), so the drain
    // flushes as it goes (a failure is visible as soon as it is recorded) and
    // honours `prova cancel`, which the main loop no longer sees.
    let drain =
        |sup: &mut Supervisor<'_>, open: &mut i32, observer: &mut dyn Observer| -> Result<bool> {
            let until = Instant::now() + DRAIN_AFTER_EXIT;
            let mut checked = Instant::now();
            while *open > 0 {
                if checked.elapsed() >= TICK {
                    checked = Instant::now();
                    sup.flush()?;
                    if store.cancel_requested(run)? {
                        return Ok(true);
                    }
                }
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
                    Chunk::Line(_, _, a) if a.at > until => break,
                    // A read boundary mid-line sends a partial too: judged by
                    // when it arrived, like a line (round 12 CI: 388 of 400).
                    Chunk::Partial(_, _, a) if a.at > until => break,
                    Chunk::Line(i, l, _) => sup.line(i, &l, observer),
                    Chunk::Partial(i, p, _) => sup.partial(i, &p, observer),
                    Chunk::Eof => *open -= 1,
                }
            }
            Ok(false)
        };
    let mut cancelled_late = drain(sup, &mut open_streams, observer)?;
    let leaked: Vec<u32> = process::sample_group(pgid).pids.into_iter().collect();
    if !leaked.is_empty() && !cancelled_late {
        process::kill_group(pgid);
        cancelled_late = drain(sup, &mut open_streams, observer)?;
    }
    if cancelled_late && terminating.is_none() {
        // Cancelled while its output was still being recorded: what was not
        // recorded yet is dropped, and the run says it was cancelled.
        sup.terminating = true;
        sup.stop_container();
        terminating = Some((Instant::now(), Status::Cancelled, "cancelled".into()));
    }
    // The test phase's size (ADR 0026 §2): what tests prova could not
    // isolate are held to. Only when the phase was sampled at least twice
    // (lines that all came at the end tell nothing about when tests ran).
    if sup.container.is_some() {
        sup.note_container();
    } else if let Some(seen) = sup.phase_seen() {
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
    // Through a container client the test process is elsewhere: say what an
    // injected report needs there (round 16: `./dev` neither forwarded
    // PYTEST_ADDOPTS nor mounted the report's directory).
    if let Some(client) = sup.container {
        for w in warnings
            .iter_mut()
            .filter(|w| w.contains("report was never written"))
        {
            w.push_str(&format!(
                "; the tests run through `{client}`: the variable must be forwarded into the container (`-e PYTEST_ADDOPTS`) and the report's directory must be mounted so the path exists inside it"
            ));
        }
    }
    // Tests quarantined by hand, as they were when this run was created (0027).
    let quarantine = crate::quarantine::in_force_at(store, store.run(run)?.created_ms)?;
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
        } else if let Some(sel) = &spec.selection
            && (sel.contains("::") || go_subtest_selection(sel))
            && sup.level > Level::Opaque
        {
            // A test named in full must itself run: a framework filter by
            // substring or by bare name can match only others (round 16:
            // `-k test_flaky` ran `test_flaky_slow`, `-run ^TestParse$`
            // ran another package's TestParse, and the run passed).
            let ran_names: Vec<String> = sup
                .lifecycle
                .outcomes()
                .keys()
                .filter(|k| !sup.superseded.contains(*k))
                .map(|k| key_display(k).to_owned())
                .collect();
            if !ran_names.iter().any(|d| names_the_selection(d, sel)) {
                let mut others = ran_names.clone();
                others.sort();
                others.truncate(5);
                contradictions.push(format!(
                    "selection {sel:?}: that test did not run; the filter matched other tests ({}): nothing selected was verified",
                    others.join(", ")
                ));
            }
        }
        if spec.selection.is_none() {
            let warned_before = warnings.len();
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
                        .then(|| {
                            sup.distiller.first_crash().map(|(_, t)| t).or_else(|| {
                                // A compiler or vet diagnostic carries no
                                // error word (round 17: `go vet`).
                                sup.tail.iter().find(|l| code_diagnostic(l)).cloned()
                            })
                        })
                        .flatten()
                        .map(|t| {
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
            // No test names to compare (quiet output), but the framework's
            // own count fell: a failing crate stops `cargo test` before the
            // next one runs (round 17: 5 counted, 9 in the passing runs).
            if let (Some(now), true) = (sup.framework_total, warnings.len() == warned_before)
                && let Some((prev, before)) = last_pass_total(store, run, &sup.suite)?
                && before > now
            {
                let message = format!(
                    "the framework counted {now} test(s), the last passing run {prev} counted {before}: {} did not run (a failing crate or step can stop the command before the others)",
                    before - now
                );
                // A pass that ran fewer tests is unverified; a failure shows
                // notes, not warnings.
                if exit_code == Some(0) {
                    warnings.push(message);
                } else {
                    sup.emit(EventKind::RunNote { message });
                }
            }
        }
    }
    // A known-flaky test left out of the command is no pass of it (0027).
    if spec.expects_tests() && status == Status::Finished && spec.selection.is_none() {
        sup.flush()?;
        if let Some(w) = left_out_flaky(store, run, &sup.suite)? {
            warnings.push(w);
        }
    }
    // Results that cannot come from this run: a test reported (or known
    // from history) to take longer than the whole run (round 9: a Makefile
    // that echoes `test x ... ok`, a stale JUnit file refreshed by touch).
    if status == Status::Finished && spec.expects_tests() {
        sup.flush()?;
        let wall = exited_ms.saturating_sub(started_ms);
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
        // Not through a container client: the tests' CPU is spent in the
        // container, out of sight (round 14).
        if status == Status::Finished
            && sup.container.is_none()
            && sup.cpu_start.is_some()
            && process::children_ticks() == sup.cpu_start
            // A framework that printed its own totals ran: a warm build's
            // tests can finish inside one clock tick (round 17).
            && sup.framework_total.unwrap_or(0) == 0
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
            let lines: Vec<String> = sup.tail.iter().cloned().collect();
            let (suspects, class) = narrow_by_running(&missing, &lines);
            let listed = list_suspects(&suspects);
            let message = match class {
                Some(c) => format!(
                    "the framework was running {c} when the run was cut short: the hung test is likely one of its {} test(s) that never reported: {listed}",
                    suspects.len(),
                ),
                None => format!(
                    "{} test(s) of the last finished run {prev} never reported before the run was cut short (a hung test is among them): {listed}",
                    missing.len(),
                ),
            };
            sup.emit(EventKind::RunNote { message });
        }
    }
    // Only quarantined tests failed, as far as the tests go: the run may
    // pass without them only if they explain everything that went wrong
    // (round 14: a build failure, a panic or a data race elsewhere hid
    // behind a quarantined failure).
    let first_seen = |d: &str| -> Option<u64> {
        store
            .conn()
            .query_row(
                "SELECT MIN(first_seen_ms) FROM tests WHERE display = ?1",
                rusqlite::params![d],
                |r| r.get::<_, Option<i64>>(0),
            )
            .ok()
            .flatten()
            .map(|v| v as u64)
    };
    // A re-keyed result counts once, under its new name (round 15: the
    // provisional Go name failed the guard of a quarantined test).
    let outcomes: BTreeMap<String, Outcome> = sup
        .lifecycle
        .outcomes()
        .iter()
        .filter(|(k, _)| !sup.superseded.contains(*k))
        .map(|(k, o)| (k.clone(), *o))
        .collect();
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
    // pytest exits 4 (usage error: a selection that names nothing) without
    // running a test: that is no test failing (round 15), like a command
    // that could not start. Exit 5 (nothing collected) stays a failure: a
    // suite that ran no test verified nothing (ADR 0017).
    if let Some(code @ 4) = exit_code
        && status == Status::Finished
        && observed == 0
        && sup.tail.iter().any(|l| pytest_ran_nothing(l))
    {
        reason = Some(format!(
            "{PYTEST_NO_TEST} (exit {code}: usage error, e.g. a selection or option pytest rejected); no test failed"
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
    let mut outcome = sup.lifecycle.verdict().unwrap_or(Outcome::Errored);
    if outcome == Outcome::Failed
        && reason
            .as_deref()
            .is_some_and(|r| r.starts_with(PYTEST_NO_TEST))
    {
        outcome = Outcome::Errored;
    }
    if outcome == Outcome::Passed {
        sup.flush()?;
        let rerun_all: Vec<String> = {
            let mut stmt = store.conn().prepare(
                // A retry: a failed attempt, then a pass of the same test.
                // A result re-keyed under another name is no attempt (round
                // 16: a quarantined Go failure read as "passed on a retry").
                "SELECT t.display FROM attempts a JOIN tests t ON t.id = a.test_id
                  WHERE a.run_id = ?1
                    AND NOT EXISTS (SELECT 1 FROM superseded s WHERE s.run_id = a.run_id AND s.test_id = a.test_id)
                  GROUP BY a.test_id
                 HAVING SUM(a.outcome IN ('failed','errored')) > 0
                    AND SUM(a.outcome = 'passed') > 0",
            )?;
            stmt.query_map(rusqlite::params![run], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        // A retried test's panic on its first try is its own (round 16:
        // a nextest retry that passed was "an error outside any failing
        // test").
        let own_crashes: Vec<String> = rerun_all.clone();
        if let Some((line, text)) = unexplained_crash(
            store,
            run,
            &sup.distiller,
            &own_crashes,
            &report.test_outputs,
        )? {
            warnings.push(format!(
                "the output shows an error outside any failing test (line {line}): {}",
                text.trim().chars().take(160).collect::<String>()
            ));
        }
        // A test that passed only on a retry is flaky: the run passes (its
        // last attempt decides, ADR 0024) but the pass is not a green one.
        let rerun = rerun_all;
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
    // Every failing test is quarantined: a known failure, said as such; the
    // run fails all the same (ADR 0034: what did not run, or ran out of
    // sight, prints nothing, so a quarantine never passes a run).
    if outcome == Outcome::Failed
        && let Some(names) = all_failures_quarantined(&outcomes, &quarantine, &first_seen)
    {
        let said = format!(
            "every failing test is quarantined: {}; the run still fails (a quarantine never passes a run: fix the test, or leave it out of the command)",
            names.join(", ")
        );
        // Another detail (Maven's `BUILD FAILURE`, a contradiction) stays
        // the detail; this is said beside it.
        if reason.is_none() {
            reason = Some(said);
        } else {
            sup.emit(EventKind::RunNote { message: said });
        }
    }
    // A failed run's missing tests are news too: a panic or a crash that
    // stopped the rest is visible only this way (round 16: two tests a
    // panic kept from running vanished without a word).
    if outcome != Outcome::Passed {
        for w in warnings
            .iter()
            .filter(|w| w.contains("from the last passing run") && w.contains("did not run"))
        {
            sup.emit(EventKind::RunNote { message: w.clone() });
        }
    }
    // A report or stream an inject recipe asked for that never appeared
    // is said whatever the outcome (round 17: a failed run behind a
    // container dropped the warning and lost its level).
    if outcome != Outcome::Passed {
        for w in warnings
            .iter()
            .filter(|w| w.contains("report was never written"))
        {
            sup.emit(EventKind::RunWarning { message: w.clone() });
        }
    }
    let no_test_failed = !sup.lifecycle.outcomes().values().any(|o| o.is_failure());
    if outcome == Outcome::Failed && no_test_failed && reason.is_none() {
        // A failure no test explains: say so, and show the first error.
        reason = Some(match sup.distiller.first_error() {
            // make's own `make: *** [..] Error 1` says only that a step
            // failed: the line above it is the step's own word (round 17).
            Some((_, text)) if let Some(step) = line_before_make_stop(&sup.tail, &text) => {
                format!(
                    "no test failed; exit {}; the step that failed said: {} (then make stopped)",
                    exit_code.unwrap_or(-1),
                    step.chars().take(200).collect::<String>()
                )
            }
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
            passed_in_iteration: false,
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
        exited_ms,
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

/// The reason prefix of a pytest run that ran no test (exit 4 or 5).
pub const PYTEST_NO_TEST: &str = "pytest ran no test";

/// A line pytest prints when it ran no test: usage errors and empty
/// collections.
fn pytest_ran_nothing(line: &str) -> bool {
    let l = line.trim();
    l.starts_with("ERROR: file or directory not found")
        || l.starts_with("ERROR: not found:")
        || l.starts_with("ERROR: usage:")
        || l.starts_with("usage: pytest")
        || l.starts_with("usage: __main__.py")
        || l.contains("no tests ran")
        || l.contains("collected 0 items")
}

/// The cause a failing test's kept output names: pytest's first `E` line,
/// or the whole output when it is one line (a short summary's message).
pub(crate) fn message_from_output(text: &str) -> Option<String> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if let Some(e) = lines.iter().find_map(|l| l.strip_prefix("E ")) {
        let e = e.trim();
        return (!e.is_empty()).then(|| e.to_owned());
    }
    match lines.as_slice() {
        [one] => Some((*one).to_owned()),
        _ => None,
    }
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

/// Go prints each package's test output only when the package ends, when
/// several packages are tested (`go test ./...`, text mode): a stalled run
/// with no test event may be that, not a hang. None for a native stream
/// (`-json`), or a command that names one package.
fn go_buffering_note(command: &str) -> Option<String> {
    let words: Vec<&str> = command.split_whitespace().collect();
    let at = words.windows(2).position(|w| w == ["go", "test"])?;
    let args = &words[at + 2..];
    if args.iter().any(|a| a.starts_with("-json")) || !args.iter().any(|a| a.contains("...")) {
        return None;
    }
    Some(
        "go buffers each package's test output until the package ends when it tests several packages, so this may be a slow package and not a hang: use `go test -json ./...` (prova reads that stream live, test by test), or `prova adopt` for a suite that does"
            .to_owned(),
    )
}

/// The suspects of a hung run as text, in the order of the last finished
/// run (the hung test is usually the first that never reported; round 17:
/// an alphabetical list cut at 8 hid it). A list cut short says how many it
/// left out.
fn list_suspects(suspects: &[&String]) -> String {
    const SHOWN: usize = 8;
    let shown: Vec<&str> = suspects.iter().take(SHOWN).map(|s| s.as_str()).collect();
    let mut text = shown.join(", ");
    if suspects.len() > shown.len() {
        text.push_str(&format!(
            " (first {} of {}; {} more not listed)",
            shown.len(),
            suspects.len(),
            suspects.len() - shown.len()
        ));
    }
    text
}

/// A failing parent is listed before its failing subtests, whichever the
/// runner reported first (round 17: full and selected runs listed them in
/// different orders).
fn parents_before_subtests(failing: &mut Vec<&(TestKey, Outcome, Option<String>, u64, bool)>) {
    let mut i = 0;
    while i < failing.len() {
        let parent = failing[i].0.display();
        if !failing[i].4
            && let Some(j) = failing[..i].iter().position(|s| {
                s.4 && s
                    .0
                    .display()
                    .strip_prefix(parent.as_str())
                    .is_some_and(|rest| rest.starts_with(['/', '[', ' ']))
            })
        {
            let f = failing.remove(i);
            failing.insert(j, f);
        }
        i += 1;
    }
}

/// The suite's last passing full run before `run` and the test count its
/// framework summary gave.
fn last_pass_total(store: &Store, run: &str, suite: &str) -> Result<Option<(String, u64)>> {
    use rusqlite::OptionalExtension;
    Ok(store
        .conn()
        .query_row(
            "SELECT r.id, CAST(json_extract(e.payload, '$.tests') AS INTEGER)
               FROM runs r JOIN events e ON e.run_id = r.id AND e.kind = 'run_totals'
              WHERE r.suite = ?1 AND r.id <> ?2 AND r.status = 'finished' AND r.outcome = 'passed'
                AND json_extract(r.spec, '$.selection') IS NULL
                AND NOT EXISTS (SELECT 1 FROM events w WHERE w.run_id = r.id AND w.kind = 'run_warning'
                                 AND w.payload LIKE '%; no evidence about the tests%')
                AND r.created_ms < (SELECT created_ms FROM runs WHERE id = ?2)
              ORDER BY r.created_ms DESC, e.seq DESC LIMIT 1",
            rusqlite::params![suite, run],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as u64)),
        )
        .optional()?)
}

/// Like `missing_since_last_pass`, from the last finished full run whatever
/// its outcome, counting the tests it skipped: the suspects of a hang
/// (round 8: a suite that never passed yet still names them).
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
         WHERE a.run_id = ?1 AND (?2 = 0 OR a.outcome <> 'skipped')
           AND NOT EXISTS (SELECT 1 FROM superseded s WHERE s.run_id = a.run_id AND s.test_id = a.test_id)
         GROUP BY a.test_id ORDER BY MIN(a.rowid)",
    )?;
    // A test the last run skipped (`TestHang` unless HANG=1) is a suspect
    // of a hang; a removed test is not one that was skipped (ADR 0031 §4).
    let earlier: Vec<(String, String)> = stmt
        .query_map(rusqlite::params![prev, passed_only as i64], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    if earlier.is_empty() {
        return Ok(None);
    }
    // What this run has, by name: a test recorded bare before and qualified
    // now (or the reverse) did run.
    let mut now: Vec<String> = Vec::new();
    for id in current {
        let d: Option<String> = store
            .conn()
            .query_row("SELECT display FROM tests WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        now.extend(d);
    }
    let missing = earlier
        .iter()
        .filter(|(id, d)| !current.contains(id) && !now.iter().any(|n| same_test_name(n, d)))
        .map(|(_, d)| d.clone())
        .collect();
    // Tests, not their subtests: the count the summary shows (round 17: "ran
    // 4 test(s)" for a run that showed 5).
    let top_level = earlier
        .iter()
        .filter(|(_, d)| !d.rsplit("::").next().unwrap_or(d).contains('/'))
        .count();
    Ok(Some((prev, missing, top_level)))
}

use crate::analysis::same_test_name;

/// The display name of a lifecycle key (`file<US>name`).
fn key_display(key: &str) -> &str {
    key.rsplit('\u{1f}').next().unwrap_or(key)
}

/// The names of the failing tests when every one of them is quarantined
/// (its own quarantine, or its parent's for a subtest known before the
/// quarantine; a parent failing only through such subtests goes with
/// them). Said in the summary; the run fails all the same (ADR 0034).
fn all_failures_quarantined(
    outcomes: &BTreeMap<String, Outcome>,
    quarantine: &BTreeMap<String, crate::quarantine::Entry>,
    first_seen: &dyn Fn(&str) -> Option<u64>,
) -> Option<Vec<String>> {
    if quarantine.is_empty() {
        return None;
    }
    let failing: Vec<&str> = outcomes
        .iter()
        .filter(|(_, o)| o.is_failure())
        .map(|(k, _)| key_display(k))
        .collect();
    let mut names = Vec::new();
    for (key, o) in outcomes {
        if !o.is_failure() {
            continue;
        }
        let display = key_display(key);
        // Of the same file, when the quarantine names one (round 16: two
        // crates' `tests::flaky`).
        let own = crate::quarantine::covers_key(quarantine, key).filter(|e| e.test == display);
        let covered = match own {
            Some(_) => true,
            None => match crate::quarantine::covers_key(quarantine, key) {
                Some(parent) => first_seen(display).is_some_and(|t| t < parent.created_ms),
                None => {
                    let prefix = format!("{display}/");
                    failing.iter().any(|f| f.starts_with(&prefix))
                }
            },
        };
        if !covered {
            return None;
        }
        names.push(display.to_owned());
    }
    (!names.is_empty()).then_some(names)
}

/// A bare Go subtest selection (`TestTable/x`): a test function, then a
/// subtest. A path (`tests/test_calc.py`) is a file, not a subtest.
fn go_subtest_selection(sel: &str) -> bool {
    let first = sel.split('/').next().unwrap_or("");
    sel.contains('/')
        && ["Test", "Example", "Fuzz", "Benchmark"]
            .iter()
            .any(|p| first.starts_with(p))
}

/// A test name the way frameworks compare them: Go writes spaces in a
/// subtest name as `_` (`x y` runs as `x_y`), and a pytest node id names a
/// file (`tests/test_calc.py::test_ok`) where prova names its module
/// (`tests.test_calc::test_ok`).
fn comparable(name: &str) -> String {
    let underscored = |s: &str| s.replace(char::is_whitespace, "_");
    match name.split_once("::") {
        Some((head, rest)) if head.ends_with(".py") => {
            let module = head.trim_end_matches(".py").replace('/', ".");
            format!("{module}::{}", underscored(rest))
        }
        _ => underscored(name),
    }
}

/// `display` against `sel`, both as `comparable` names: the selected test
/// itself, one of its subtests, or one of its parameterized cases. A
/// pytest selection that gives only the end of the module path
/// (`test_calc.py::test_ok`) names the module's tail.
fn names_comparable(display: &str, sel: &str) -> bool {
    let whole = |d: &str, s: &str| {
        d == s
            || ["/", "[", "("]
                .iter()
                .any(|p| d.strip_prefix(s).is_some_and(|r| r.starts_with(p)))
    };
    if whole(display, sel) {
        return true;
    }
    match (display.split_once("::"), sel.split_once("::")) {
        (Some((dh, dr)), Some((sh, sr))) if !sh.contains('/') => {
            (dh == sh || dh.ends_with(&format!(".{sh}"))) && whole(dr, sr)
        }
        _ => false,
    }
}

/// Whether a test's display name is the selected test, one of its
/// subtests, or one of its parameterized cases. A parent that ran is not
/// its subtest: a subtest that does not exist leaves only the parent
/// (round 17: `TestTable/x` passed on the parent alone).
fn names_the_selection(display: &str, sel: &str) -> bool {
    let (d, s) = (comparable(display), comparable(sel));
    names_comparable(&d, &s)
        // A selection without a package or file (`TestTable/x`) names the
        // part of the display after it.
        || (!s.contains("::")
            && d.split_once("::").is_some_and(|(_, r)| names_comparable(r, &s)))
}

/// Whether a test that ran belongs to the selection, for the note about
/// tests outside it: the selected test and its subtests, a parent of the
/// selection, any test of a selected file, or (for a selection that is a
/// bare name) a test that contains it.
fn in_selection(t: &str, sel: &str) -> bool {
    let (td, sd) = (comparable(t).to_lowercase(), comparable(sel).to_lowercase());
    let parent = |p: &str, s: &str| s.starts_with(&format!("{p}/"));
    if sel.contains("::") && t.contains("::") {
        return names_comparable(&td, &sd) || parent(&td, &sd);
    }
    // A file as the selection (`test_fixtures.py`, `tests/test_fixtures.py`):
    // every test of that module.
    if let Some(file) = sel.strip_suffix(".py") {
        let module = file.replace('/', ".");
        if let Some((head, _)) = t.split_once("::") {
            return head == module || head.ends_with(&format!(".{module}"));
        }
    }
    let leaf = sd.rsplit("::").next().unwrap_or(&sd).to_owned();
    td.contains(&leaf) || leaf.contains(td.rsplit("::").next().unwrap_or(&td))
}

/// Whether `name` occurs in `line` as a whole name, not inside a longer one
/// (round 15: `test_q` covered a crash of `test_queue`).
fn names_word(line: &str, name: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    line.match_indices(name).any(|(i, _)| {
        let before = line[..i].chars().next_back();
        let after = line[i + name.len()..].chars().next();
        !before.is_some_and(word) && !after.is_some_and(word)
    })
}

/// The ways a framework prints a test's name: the whole display, without
/// its file or binary part, its last part, and Jest's `›` for ` > `.
fn printed_names(display: &str) -> Vec<String> {
    let mut out = vec![display.to_owned()];
    if let Some((_, rest)) = display.split_once("::") {
        out.push(rest.to_owned());
    }
    let last = display
        .rsplit(['.', ':'])
        .next()
        .unwrap_or(display)
        .rsplit(" > ")
        .next()
        .unwrap_or(display);
    out.push(last.to_owned());
    let more: Vec<String> = out
        .iter()
        .filter(|n| n.contains(" > "))
        .map(|n| n.replace(" > ", " \u{203a} "))
        .collect();
    out.extend(more);
    out.retain(|n| n.len() >= 3);
    out.sort();
    out.dedup();
    out
}

/// Whether `line` is a compiler, vet or linter diagnostic of the project's
/// code: `path/file.ext:LINE:COL: message` (or `:LINE: message`).
fn code_diagnostic(line: &str) -> bool {
    let mut parts = line.trim().splitn(4, ':');
    let (Some(path), Some(l), Some(c)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let number = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    let has_path = !path.is_empty()
        && !path.contains(char::is_whitespace)
        && path.rsplit_once('.').is_some_and(|(_, ext)| {
            !ext.is_empty() && ext.chars().all(|c| c.is_ascii_alphanumeric())
        });
    has_path && number(l) && (number(c) || c.starts_with(' '))
}

/// Whether `line` is make's own wrap-up of a failed step.
fn is_make_wrapup(line: &str) -> bool {
    let t = line.trim_start();
    (t.starts_with("make") && t.contains("*** "))
        || (t.starts_with("make") && t.contains("Target '") && t.contains("not remade"))
        || (t.starts_with("make")
            && (t.contains("Entering directory") || t.contains("Leaving directory")))
}

/// When `error` is make's own stop, the nearest output line above it that
/// is not make's: the failed step's own word.
fn line_before_make_stop(tail: &VecDeque<String>, error: &str) -> Option<String> {
    if !is_make_wrapup(error) {
        return None;
    }
    let at = tail.iter().rposition(|l| l.trim() == error.trim())?;
    tail.iter()
        .take(at)
        .rev()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !is_make_wrapup(l))
        .map(str::to_owned)
}

/// The first crash-like line of the output that none of `tests` (the
/// tests that passed on a retry) accounts for: it names one of them, or it
/// is part of one's own output or failure message. With none, the first
/// crash.
fn unexplained_crash(
    store: &Store,
    run: &str,
    distiller: &crate::output::Distiller,
    quarantined: &[String],
    outputs: &[(TestKey, String)],
) -> Result<Option<(u64, String)>> {
    let crashes = distiller.crashes();
    if quarantined.is_empty() || crashes.is_empty() {
        return Ok(crashes.into_iter().next());
    }
    let mut own: Vec<String> = outputs
        .iter()
        .filter(|(k, _)| quarantined.contains(&k.display()))
        .map(|(_, o)| o.clone())
        .collect();
    let mut stmt = store.conn().prepare(
        "SELECT a.message FROM attempts a JOIN tests t ON t.id = a.test_id
          WHERE a.run_id = ?1 AND t.display = ?2 AND a.message IS NOT NULL",
    )?;
    for name in quarantined {
        for m in stmt.query_map(rusqlite::params![run, name], |r| r.get::<_, String>(0))? {
            own.push(m?);
        }
    }
    // A name as frameworks print it: the whole display, or without the
    // binary or file part before the first `::`.
    let names: Vec<String> = quarantined.iter().flat_map(|n| printed_names(n)).collect();
    Ok(crashes.into_iter().find(|(_, line)| {
        let t = line.trim();
        !names.iter().any(|n| names_word(t, n))
            && !own.iter().any(|o| o.contains(t))
            && !runner_failure_summary(t)
    }))
}

/// A test runner's own line saying that tests failed (nextest's `error: test
/// run failed`, cargo's `error: test failed, to rerun pass ...`): it says
/// nothing beyond the failures already seen (round 15: it failed every
/// quarantined pass under nextest).
fn runner_failure_summary(line: &str) -> bool {
    line == "error: test run failed"
        || line.starts_with("error: test failed, to rerun pass")
        // One target failed; `error: 2 targets failed:` is another's too.
        || line == "error: 1 target failed:"
        // Maven's wrap-up of a failed test step.
        || line == "[INFO] BUILD FAILURE"
}

/// Whether the tests of `run` share a test with any of `suites`: a suite
/// that runs part of what another suite runs (0027 §6).
fn overlaps(store: &Store, run: &str, suites: &[String]) -> Result<bool> {
    for suite in suites {
        let hit: bool = store.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM attempts a JOIN tests t1 ON t1.id = a.test_id
                              JOIN tests t2 ON t2.display = t1.display
                             WHERE a.run_id = ?1 AND t2.suite = ?2)",
            rusqlite::params![run, suite],
            |r| r.get(0),
        )?;
        if hit {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The warning for known-flaky tests that have no result in this full run
/// although this suite, or a suite sharing tests with it, ran them: leaving a
/// flaky test out of the command proves nothing about it (ADR 0027 §6).
/// Quarantined tests too: a quarantine never makes a run look better (0034).
fn left_out_flaky(store: &Store, run: &str, suite: &str) -> Result<Option<String>> {
    let ran: std::collections::BTreeSet<String> = {
        let mut stmt = store.conn().prepare(
            "SELECT DISTINCT t.display FROM attempts a JOIN tests t ON t.id = a.test_id WHERE a.run_id = ?1",
        )?;
        stmt.query_map(rusqlite::params![run], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    if ran.is_empty() {
        return Ok(None);
    }
    let mut left: Vec<String> = Vec::new();
    for f in crate::analysis::flaky(store, None, 50)? {
        // A name recorded bare (`TestX`, text output before Go results were
        // qualified by package) is the test `pkg::TestX` this run has.
        if ran.contains(&f.display)
            || ran.iter().any(|r| same_test_name(r, &f.display))
            || left.contains(&f.display)
        {
            continue;
        }
        // Another suite sharing tests with this one is evidence only when
        // the flaky test's neighbours (its file, or its module) ran here: a
        // suite that runs everything shares tests with a narrow one that
        // never meant to run it (round 15: `small` warned about a flaky
        // test of `medium`, because `spec` runs both).
        if f.suites.iter().any(|s| s == suite)
            || (overlaps(store, run, &f.suites)? && neighbour_ran(store, run, &f.display)?)
        {
            left.push(f.display);
        }
    }
    if left.is_empty() {
        return Ok(None);
    }
    left.sort();
    Ok(Some(format!(
        "{} known flaky test(s) did not run: {}{}; leaving a flaky test out of the command hides it: run it, and fix it (a quarantine marks it in the open but does not clear this warning)",
        left.len(),
        left.iter().take(5).cloned().collect::<Vec<_>>().join(", "),
        if left.len() > 5 { ", ..." } else { "" }
    )))
}

/// Whether this run recorded a test of the same file (or, with no file, the
/// same module path) as the test named `display`.
fn neighbour_ran(store: &Store, run: &str, display: &str) -> Result<bool> {
    Ok(store.conn().query_row(
        "SELECT EXISTS (SELECT 1 FROM attempts a JOIN tests t ON t.id = a.test_id
                         WHERE a.run_id = ?1
                           AND EXISTS (SELECT 1 FROM tests f WHERE f.display = ?2
                                         AND ((f.file IS NOT NULL AND f.file = t.file)
                                           OR (f.file IS NULL AND t.file IS NULL
                                               AND f.path = t.path AND f.path <> '[]'))))",
        rusqlite::params![run, display],
        |r| r.get(0),
    )?)
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
    /// The test binary, when another test of the same name lives elsewhere
    /// (ADR 0031 section 2): `FAIL tests::flaky (geo)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    /// A known failure: the test is quarantined. Listed all the same; it
    /// fails the run like any other (ADR 0034).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub quarantined: bool,
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
    /// The test binary (crate) the name does not carry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
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
    /// Of them, skipped (round 14: a skipped subtest read as one that ran).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub subtests_skipped: u64,
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
    /// Some results came from result files although the level is higher
    /// (round 14: "recognized output" with counts read from TRX).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub with_result_files: bool,
    pub last_seq: u64,
    pub elapsed_ms: u64,
    pub counts: Counts,
    /// Tests running now (started, not finished), oldest first; at most 10.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub running: Vec<String>,
    /// How long each test of `running` has been running, in order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub running_for_ms: Vec<u64>,
    /// Tests skipped in a full run, by name (at most 10): a count alone
    /// hides which (round 16).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped_tests: Vec<String>,
    /// A stalled run where no test is seen running: the tests of the last
    /// finished run that have not reported yet, whole and in that run's
    /// order (the text lists at most 8; ADR 0031 §4).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub not_reported: Vec<String>,
    /// The run's selection (`--select`), if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection: Option<String>,
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
    /// The suite has a `select` template, so `--select` works with it.
    #[serde(skip)]
    pub can_select: bool,
    /// History of the same suite, filled in by the caller when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<serde_json::Value>,
    /// Tests quarantined by `prova quarantine` that this run touched or could
    /// have run, with their result here (ADR 0027): never hidden.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub quarantined: Vec<QuarantinedTest>,
    /// The slowest tests of this finished run, 1 s or more, worst first.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub slowest: Vec<SlowestTest>,
}

/// One quarantined test as a run saw it.
#[derive(Debug, Clone, Serialize)]
pub struct QuarantinedTest {
    pub test: String,
    /// The test binary of an `--in` quarantine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    /// `passed`, `failed`, `errored`, `skipped`, `not run`, or `not
    /// reported` (no per-test results, or the run was cut short).
    pub result: String,
    pub reason: String,
    pub expires_ms: u64,
}

/// A test of the run that took long.
#[derive(Debug, Clone, Serialize)]
pub struct SlowestTest {
    pub test: String,
    pub duration_ms: u64,
}

/// A test this slow (ms) is named in the summary (ADR 0027 §8).
const SLOWEST_MIN_MS: i64 = 1000;

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
    // A quarantined test's failure is no reason to wake up (0027 §5).
    let quarantine = crate::quarantine::for_run(store, run).unwrap_or_default();
    loop {
        let v = store.data_version()?;
        if v != version {
            version = v;
            let events = store.events_after(run, cursor)?;
            for e in &events {
                cursor = e.seq;
                if let Some(reason) = matches(until, &e.kind) {
                    if reason == WaitReason::Failure
                        && let EventKind::TestFinished { test, .. } = &e.kind
                        && crate::quarantine::covers_key(&quarantine, &test.unique()).is_some()
                    {
                        continue;
                    }
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
/// The suspects of a hang narrowed by the framework's own last `Running
/// <class>` line (Maven Surefire, `[INFO] Running calc.CalcTest`): the
/// hung test is in the class it was running, not anywhere in the last run
/// (round 13: the note listed tests that had not even started).
fn narrow_by_running<'a>(
    missing: &'a [String],
    lines: &[String],
) -> (Vec<&'a String>, Option<String>) {
    let running = lines.iter().rev().find_map(|l| {
        let t = l.trim().trim_start_matches("[INFO]").trim();
        let name = t.strip_prefix("Running ")?.split_whitespace().next()?;
        (name.contains('.') && !name.contains('/')).then(|| name.to_owned())
    });
    if let Some(class) = &running {
        let narrowed: Vec<&String> = missing
            .iter()
            .filter(|m| m.starts_with(&format!("{class}::")) || m.starts_with(&format!("{class}.")))
            .collect();
        if !narrowed.is_empty() {
            return (narrowed, running);
        }
    }
    (missing.iter().collect(), None)
}

/// The excerpt as a person reads it: `go test -json` lines become their
/// own output text (round 13: 30 lines of raw JSON under a failure prova
/// had already decoded).
fn readable(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .filter_map(|l| {
            let t = l.trim_start();
            if !t.starts_with('{') {
                return Some(l);
            }
            match serde_json::from_str::<serde_json::Value>(t) {
                Ok(v) if v.get("Action").is_some() => v["Output"]
                    .as_str()
                    .map(|o| o.trim_end().to_owned())
                    .filter(|o| !o.trim().is_empty()),
                _ => Some(l),
            }
        })
        .collect()
}

/// The part of a filter a framework's "not found" line names, when the
/// filter has spaces and the line quotes only a piece of it: a wrapper or
/// shell split it into words (round 15: `./dev` re-splits `$*`).
pub fn split_filter_fragment(filter: &str, lines: &[&String]) -> Option<String> {
    if !filter.contains(' ') {
        return None;
    }
    lines.iter().find_map(|l| {
        let (_, frag) = l.split_once("not found:")?;
        let frag = frag.trim();
        (!frag.is_empty() && frag != filter && filter.contains(frag)).then(|| frag.to_owned())
    })
}

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

/// Runs whose pass verified little: a warning (ADR 0017), or a known flaky
/// test that passed. The run list marks them `passed?`.
pub fn unverified_runs(
    store: &Store,
    runs: &[RunRow],
) -> Result<std::collections::BTreeSet<String>> {
    let ids: Vec<String> = runs.iter().map(|r| r.id.clone()).collect();
    let mut out = store.unverified(&ids)?;
    for r in runs {
        if r.status == Status::Finished
            && r.outcome == Some(Outcome::Passed)
            && !out.contains(&r.id)
            && !flaky_passed_in(store, r)?.is_empty()
        {
            out.insert(r.id.clone());
        }
    }
    Ok(out)
}

/// Known flaky tests (as of the run) that passed in a finished run,
/// quarantined ones included (ADR 0034): such a pass proves little (round 7). Read from the
/// store alone; it once took a full summary of the run, whose history took
/// summaries of earlier runs in turn, exponential in the number of passes
/// (round 14: `prova status` used a second of CPU, `wait` a core).
fn flaky_passed_in(store: &Store, row: &RunRow) -> Result<Vec<String>> {
    let passed: std::collections::BTreeSet<String> = {
        let mut stmt = store.conn().prepare(
            "SELECT DISTINCT t.display FROM attempts a JOIN tests t ON t.id = a.test_id
              WHERE a.run_id = ?1 AND a.outcome = 'passed'
                AND a.attempt = (SELECT MAX(b.attempt) FROM attempts b
                                  WHERE b.run_id = a.run_id AND b.test_id = a.test_id)
                AND NOT EXISTS (SELECT 1 FROM superseded s
                                 WHERE s.run_id = a.run_id AND s.test_id = a.test_id)",
        )?;
        stmt.query_map(rusqlite::params![row.id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    let mut out = Vec::new();
    if passed.is_empty() {
        return Ok(out);
    }
    for f in crate::analysis::flaky_as_of(store, None, 50, Some(&row.id))? {
        if passed.contains(&f.display) && !out.contains(&f.display) {
            out.push(f.display);
        }
    }
    Ok(out)
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
                    place: test.place().map(str::to_owned),
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
            // Idle for as long as the stall said plus the time since, while
            // nothing else happened (round 14: "no activity for 5.1s" half
            // a minute later).
            EventKind::RunStalled { idle_ms, .. } => {
                stalled_ms = Some(if row.status.is_terminal() {
                    *idle_ms
                } else {
                    idle_ms + now_ms().saturating_sub(e.ts_ms)
                })
            }
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
            EventKind::TestSuperseded { test, .. } => {
                finals.remove(&test.unique());
            }
            _ => {}
        }
    }
    let mut counts = Counts::default();
    let mut failing: Vec<&(TestKey, Outcome, Option<String>, u64, bool)> = Vec::new();
    for f in finals.values() {
        if f.4 {
            counts.subtests += 1;
            counts.subtests_failed += u64::from(f.1.is_failure());
            counts.subtests_skipped += u64::from(f.1 == Outcome::Skipped);
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
    parents_before_subtests(&mut failing);
    // Quarantined tests are also listed apart, with their result (ADR 0027).
    let quarantine = crate::quarantine::in_force_at(store, row.created_ms)?;
    let first_seen = |d: &str| -> Option<u64> {
        store
            .conn()
            .query_row(
                "SELECT MIN(first_seen_ms) FROM tests WHERE display = ?1",
                rusqlite::params![d],
                |r| r.get::<_, Option<i64>>(0),
            )
            .ok()
            .flatten()
            .map(|v| v as u64)
    };
    // Quarantined failures stay in the list, marked: failures come first,
    // known or not (ADR 0034). A subtest first seen after its parent's
    // quarantine is news, not the parent's known failure (round 17).
    let is_quarantined = |key: &TestKey| {
        let display = key.display();
        match crate::quarantine::covers_key(&quarantine, &key.unique()) {
            None => false,
            Some(p) if p.test == display => true,
            Some(p) => first_seen(&display).is_some_and(|t| t < p.created_ms),
        }
    };
    // Did the same suite (same selection) pass on exactly this code before?
    let selection = serde_json::from_value::<RunSpec>(row.spec.clone())
        .ok()
        .and_then(|s| s.selection);
    // Only a run that finished can suggest flakiness: a cancelled or aborted
    // run's unfinished tests say nothing about the code (ADR 0018).
    // Only a verified pass is evidence the suite can pass on this code
    // (round 11: an echoed pass made a real failure "likely flaky").
    // Like with like (ADR 0035 section 2): a pass that set quarantined
    // failures aside (exit code not 0) did not pass on this code, and a
    // pass is evidence about a test only if that run ran the test.
    let passes_on_same_code: Vec<String> = if row.status == Status::Finished {
        let prev = store.suite_history(&row.suite, selection.as_deref(), row.created_ms, 200)?;
        let ids: Vec<String> = prev.iter().map(|r| r.id.clone()).collect();
        let thin = store.unverified(&ids)?;
        prev.iter()
            .filter(|r| {
                r.code == row.code
                    && r.outcome == Some(Outcome::Passed)
                    && r.exit_code == Some(0)
                    && !thin.contains(&r.id)
            })
            .map(|r| r.id.clone())
            .collect()
    } else {
        Vec::new()
    };
    let suite_passed_ran = |display: &str| -> bool {
        passes_on_same_code.iter().any(|r| {
            store
                .conn()
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM attempts a JOIN tests t ON t.id = a.test_id
                      WHERE a.run_id = ?1 AND t.display = ?2 AND a.outcome = 'passed')",
                    rusqlite::params![r, display],
                    |q| q.get::<_, bool>(0),
                )
                .unwrap_or(false)
        })
    };
    let mut failures = Vec::new();
    // A failure reported before its message (pytest -v prints the line
    // first, the `-rfE` summary names the cause at the end): the message is
    // in the test's kept output (round 15).
    let kept: BTreeMap<String, String> =
        if row.status.is_terminal() && failing.iter().take(20).any(|f| f.2.is_none()) {
            crate::output::test_outputs(store, run)?
                .into_iter()
                .collect()
        } else {
            BTreeMap::new()
        };
    for (key, outcome, message, _, _) in failing.iter().take(20).copied() {
        let id = crate::store::test_id(&row.suite, key);
        let message = message.clone().or_else(|| {
            kept.get(&key.display())
                .and_then(|t| message_from_output(t))
        });
        let shared = finals
            .values()
            .any(|f| f.0.display() == key.display() && f.0.unique() != key.unique())
            || crate::quarantine::files_of(store, &key.display())
                .map(|f| f.len() > 1)
                .unwrap_or(false);
        failures.push(Failure {
            test: key.display(),
            place: key.place().filter(|_| shared).map(str::to_owned),
            quarantined: is_quarantined(key),
            outcome: *outcome,
            message: message.or_else(|| crate::output::test_output_summary(store, run, &id)),
            context: crate::analysis::failure_context(store, &id, run)?.map(|mut c| {
                c.suite_passed_on_same_code = suite_passed_ran(&key.display());
                c
            }),
        });
    }
    let n_failures = failing.len();
    // A pass of a known flaky test proves little: say so, or a green run
    // reads as a reliably green suite (round 7).
    let flaky_passed = if row.status == Status::Finished {
        flaky_passed_in(store, &row)?
    } else {
        Vec::new()
    };
    // Quarantined tests of this run, with their result: relevant when they
    // ran here, or their suite (or one sharing tests with this run) did.
    let mut quarantined: Vec<QuarantinedTest> = Vec::new();
    // While the run goes on, the quarantined tests with a result so far:
    // a "1 failed" that does not end a first-failure wait is explained
    // (round 14).
    let live = !row.status.is_terminal();
    {
        // Without per-test results (opaque level, or a run cut short) a
        // missing quarantined test is not known to have been left out
        // (round 14: "not run" in a cancelled run).
        let blind = row.level < Level::Recognized || row.status != Status::Finished;
        // Quiet output (pytest -q) counts tests without naming the passing
        // ones: a quarantined test with no result there was not seen, which
        // is not "not run" (round 17).
        let quiet = row.status == Status::Finished
            && framework.is_some_and(|(t, _)| t > 0)
            && !finals.values().any(|f| f.1 == Outcome::Passed);
        for q in quarantine.values() {
            let name = &q.test;
            // A quarantined subtest is listed by its own result (round 14:
            // "not run" for a subtest that ran and failed).
            let result = finals
                .values()
                .filter(|f| f.0.display() == *name && !f.0.place_unknown())
                .filter(|f| match (f.0.place(), &q.file) {
                    (Some(p), Some(q)) => p == q,
                    // A result of unknown place is not that place's when
                    // the name is shared (ADR 0035 section 1).
                    (None, Some(_)) => !q.ambiguous,
                    _ => true,
                })
                .map(|f| f.1.as_str().to_owned())
                .next()
                // A test of that name whose directory could not be told is
                // not the quarantined one: say what it did (round 17: it
                // read "not seen").
                .or_else(|| {
                    finals
                        .values()
                        .find(|f| f.0.display() == *name && f.0.place_unknown())
                        .map(|f| format!("{} in an unknown directory", f.1.as_str()))
                });
            // A selection that did not include it says nothing of it
            // (round 15: "not run" on every --select run).
            if (live || selection.is_some()) && result.is_none() {
                continue;
            }
            if result.is_none() && !(blind && row.level < Level::Recognized) {
                let suites: Vec<String> = {
                    let mut stmt = store
                        .conn()
                        .prepare("SELECT DISTINCT suite FROM tests WHERE display = ?1")?;
                    stmt.query_map(rusqlite::params![name], |r| r.get(0))?
                        .collect::<rusqlite::Result<_>>()?
                };
                if !suites.contains(&row.suite) && !overlaps(store, run, &suites)? {
                    continue;
                }
            }
            quarantined.push(QuarantinedTest {
                test: name.clone(),
                place: q.file.clone(),
                result: result.unwrap_or_else(|| {
                    if blind {
                        "not reported"
                    } else if quiet {
                        "not seen (quiet output)"
                    } else {
                        "not run"
                    }
                    .to_owned()
                }),
                reason: q.reason.clone(),
                expires_ms: q.expires_ms,
            });
        }
        // A quarantine that ran out no longer marks its test: say so.
        for q in crate::quarantine::expired_at(store, row.created_ms)?
            .into_iter()
            .filter(|_| !live)
        {
            if finals.values().any(|f| !f.4 && f.0.display() == q.test) {
                notes.push(format!(
                    "quarantine of {} expired ({}); it counts again: fix it, or `prova quarantine add` it again with a reason",
                    q.test, q.reason
                ));
            }
        }
    }
    // The slowest tests of a finished run (ADR 0027 §8).
    let slowest: Vec<SlowestTest> = if row.status.is_terminal() {
        let mut stmt = store.conn().prepare(
            "SELECT t.display, MAX(a.duration_ms) AS d FROM attempts a JOIN tests t ON t.id = a.test_id
              WHERE a.run_id = ?1 AND a.duration_ms >= ?2
                AND NOT EXISTS (SELECT 1 FROM superseded s WHERE s.run_id = a.run_id AND s.test_id = a.test_id)
                -- A test the cancel cut off took as long as the cancel let it
                -- (round 16: a hung test listed as slow).
                AND NOT (a.outcome = 'errored' AND ?3)
              GROUP BY t.display ORDER BY d DESC, t.display LIMIT 3",
        )?;
        stmt.query_map(
            rusqlite::params![run, SLOWEST_MIN_MS, row.status != Status::Finished],
            |r| {
                Ok(SlowestTest {
                    test: r.get(0)?,
                    duration_ms: r.get::<_, i64>(1)? as u64,
                })
            },
        )?
        .collect::<rusqlite::Result<_>>()?
    } else {
        Vec::new()
    };
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
    // A selected run's failure outside the selection: a file-level entry
    // the framework loads whatever the selection (Vitest imports every
    // file for `-t`), or a test the filter did not name (round 15: read as
    // the selection leaking).
    if let Some(sel) = &selection {
        // A qualified selection (`pkg::TestAdd`) names one test and its
        // subtests: a same-named test of another package that ran is
        // outside it (round 16: `go test -run ^TestAdd$ ./...` ran both).
        let qualified = sel.contains("::");
        let selected = |t: &str| in_selection(t, sel);
        let mut outside: Vec<String> = finals
            .values()
            // A test the filter skipped did not run (Vitest `-t` reports
            // every other test as skipped).
            .filter(|f| f.1 != Outcome::Skipped)
            .map(|f| f.0.display())
            .filter(|t| {
                t.ends_with("::(file)")
                    || t.ends_with("::(package)")
                    || t.ends_with("::(suite)")
                    || !selected(t)
            })
            .filter(|t| {
                // Passes outside only matter for a qualified selection;
                // failures always.
                qualified || failures.iter().any(|f| f.test == *t)
            })
            .collect();
        outside.sort();
        outside.dedup();
        let outside: Vec<&str> = outside.iter().map(String::as_str).collect();
        if !outside.is_empty() {
            notes.push(format!(
                "outside the selection {sel:?}: {} (the framework loaded or ran more than the filter names)",
                outside.join(", ")
            ));
        }
    }
    // A failure before any test ran may be the environment's (a missing
    // SDK), not the code's; history would count it against the code
    // (round 14). Say how to keep it out.
    if row.status == Status::Finished
        && matches!(row.outcome, Some(Outcome::Failed | Outcome::Errored))
        && counts.results() == 0
        // Quiet output: the framework counted tests, so tests ran and the
        // failure came after them (round 16: a lint step's failure got a
        // `forget` hint).
        && framework.is_none_or(|(t, _)| t == 0)
        // A diagnostic of the code (`vetbad.go:6:57: ...`) is a code failure,
        // not the environment's (round 17: the hint invited erasing it).
        && !output_tail(store, run, 40).iter().any(|l| code_diagnostic(l))
    {
        notes.push(format!(
            "no test result was seen: if this failed for the environment (a missing tool or SDK), not the code, `prova forget {run} --reason \"...\"` keeps it out of history"
        ));
    }
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
    let mut not_reported: Vec<String> = Vec::new();
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
            let lines = output_tail(store, run, 30);
            let (suspects, class) = narrow_by_running(&missing, &lines);
            let listed = list_suspects(&suspects);
            not_reported = missing.clone();
            notes.push(match class {
                Some(c) => format!(
                    "the framework was running {c}: the hung test is likely one of its {} test(s) not reported yet: {listed}",
                    suspects.len(),
                ),
                None => format!(
                    "not reported yet ({} of the last finished run {prev}; the hung test is likely among them): {listed}",
                    missing.len(),
                ),
            });
        }
    }
    // Go buffers a package's text output until the package ends.
    if !row.status.is_terminal()
        && stalled_ms.is_some()
        && running.is_empty()
        && counts.results() == 0
        && let Some(n) = go_buffering_note(&command)
    {
        notes.push(n);
    }
    // A selection with spaces that a wrapper split into words (round 16:
    // `./dev` re-splits `$*`; `prova adopt --verify` said so, `run` did not).
    if let Some(sel) = &selection
        && row.status.is_terminal()
        && counts.results() == 0
    {
        let lines = output_tail(store, run, 30);
        let refs: Vec<&String> = lines.iter().collect();
        if let Some(frag) = split_filter_fragment(sel, &refs) {
            notes.push(format!(
                "the filter {sel:?} was split into words by a wrapper or the shell ({frag:?} is only a part of it): use a filter without spaces (pytest: `-k` with a substring)"
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
        // The framework already said no test ran: one statement, not a
        // second that contradicts it (round 13, Rust).
        && !notes.iter().any(|n| n.contains("no test ran"))
    {
        notes.push(format!(
            "selection {sel:?}: no per-test results seen, so prova cannot tell whether any test matched"
        ));
    }
    // A `next:` that suggests --select must not fail with a usage error
    // (round 13: `make` has no `select`).
    let can_select = crate::config::load(store.root())
        .ok()
        .and_then(|c| c.suite.get(&row.suite).map(|s| s.select.is_some()))
        .unwrap_or(false);
    Ok(Summary {
        can_select,
        run: row.id.clone(),
        suite: row.suite.clone(),
        command,
        reason,
        status: row.status,
        outcome: row.outcome,
        level: row.level,
        with_result_files: row.level > Level::ResultFiles
            && store
                .conn()
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM attempts WHERE run_id = ?1 AND source = ?2)",
                    rusqlite::params![run, Level::ResultFiles.as_u8()],
                    |r| r.get::<_, bool>(0),
                )
                .unwrap_or(false),
        last_seq,
        elapsed_ms: end.saturating_sub(row.started_ms.unwrap_or(row.created_ms)),
        counts,
        // How long each has been running: a test running for 30 s that
        // usually takes milliseconds is the hang, before any budget says so
        // (round 14).
        selection: selection.clone(),
        not_reported,
        skipped_tests: if selection.is_none() {
            let mut v: Vec<String> = finals
                .values()
                .filter(|f| f.1 == Outcome::Skipped && !f.4)
                .map(|f| match f.2.as_deref() {
                    Some("xfail") => format!("{} (xfail: expected to fail)", f.0.display()),
                    _ => f.0.display(),
                })
                .collect();
            v.sort();
            v.truncate(10);
            v
        } else {
            Vec::new()
        },
        running_for_ms: if row.status.is_terminal() {
            Vec::new()
        } else {
            let now = now_ms();
            running
                .iter()
                .take(10)
                .map(|(u, _)| started_at.get(u).map_or(0, |t| now.saturating_sub(*t)))
                .collect()
        },
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
        quarantined,
        slowest,
        output_tail: if want_tail {
            readable(output_tail(store, run, 30))
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
    // A pass that relied on a known flaky test is no "last green" either
    // (round 13: it named a run whose own output said "unverified").
    // At most this many recent passes are checked for one: each check
    // reads a test's flake history.
    const CHECKED_PASSES: usize = 20;
    let mut last_passed = None;
    let mut flaky_passes = std::collections::BTreeSet::new();
    for r in prev
        .iter()
        .filter(|r| {
            r.status == Status::Finished
                && r.outcome == Some(Outcome::Passed)
                // A pass recorded before ADR 0034 with a quarantined failure
                // set aside is no "last green" (round 15).
                && r.exit_code == Some(0)
                && !thin.contains(&r.id)
        })
        .take(CHECKED_PASSES)
    {
        if flaky_passed_in(store, r)?.is_empty() {
            last_passed = Some(r);
            break;
        }
        flaky_passes.insert(r.id.clone());
    }
    // A pass that relied on a known flaky test is a pass with a warning,
    // never "none of them passed" (round 14).
    let thin: std::collections::BTreeSet<String> = thin.union(&flaky_passes).cloned().collect();
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
            // A failure with no test result says nothing of a test (round
            // 14: a missing SDK read as "on this exact code: failed").
            Status::Finished
                if r.outcome != Some(Outcome::Passed)
                    && !store
                        .conn()
                        .query_row(
                            "SELECT EXISTS (SELECT 1 FROM attempts WHERE run_id = ?1)",
                            rusqlite::params![r.id],
                            |q| q.get::<_, bool>(0),
                        )
                        .unwrap_or(true) =>
            {
                format!(
                    "{} (no test seen)",
                    r.outcome.map(|o| o.as_str()).unwrap_or_default()
                )
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
        // Passes recorded before ADR 0034 with a quarantined failure set
        // aside: no "last green", but passes all the same (round 16).
        "set_aside_passes": prev
            .iter()
            .filter(|r| {
                r.status == Status::Finished
                    && r.outcome == Some(Outcome::Passed)
                    && r.exit_code.is_some_and(|c| c != 0)
            })
            .map(|r| r.id.clone())
            .collect::<Vec<_>>(),
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
                       -- a suite that did not run the failing test says
                       -- nothing about it (round 18)
                       AND (NOT EXISTS (SELECT 1 FROM attempts fa WHERE fa.run_id = ?3 AND fa.outcome IN ('failed', 'errored'))
                            OR EXISTS (SELECT 1 FROM attempts pa JOIN tests pt ON pt.id = pa.test_id
                                        JOIN tests ft ON ft.display = pt.display
                                        JOIN attempts fa ON fa.test_id = ft.id
                                       WHERE pa.run_id = r.id AND pa.outcome = 'passed'
                                         AND fa.run_id = ?3 AND fa.outcome IN ('failed', 'errored')))
                       AND r.exit_code = 0
                     ORDER BY created_ms DESC LIMIT 1",
                    rusqlite::params![row.code, row.suite, row.id],
                    |r| Ok(serde_json::json!({ "run": r.get::<_, String>(0)?, "suite": r.get::<_, String>(1)? })),
                )
                .ok()
        } else {
            None
        },
    })))
}

#[cfg(test)]
mod quarantine_tests {
    use super::*;
    use crate::quarantine::Entry;

    fn q(names: &[&str]) -> BTreeMap<String, Entry> {
        names
            .iter()
            .map(|n| {
                (
                    (*n).to_owned(),
                    Entry {
                        test: (*n).to_owned(),
                        reason: "r".into(),
                        created_ms: 0,
                        expires_ms: 1,
                        removed_ms: None,
                        file: None,
                        ambiguous: false,
                    },
                )
            })
            .collect()
    }

    fn outcomes(list: &[(&str, Outcome)]) -> BTreeMap<String, Outcome> {
        list.iter()
            .map(|(k, o)| (format!("\u{1f}{k}"), *o))
            .collect()
    }

    /// Which failures the summary may call all quarantined (ADR 0034: the
    /// run fails either way). A subtest follows its parent's quarantine
    /// only when it was known before it (round 14); a parent failing only
    /// through quarantined subtests goes with them; another failure, or no
    /// quarantine, names none.
    #[test]
    fn every_failure_quarantined_is_named_and_nothing_else() {
        let one = outcomes(&[("a", Outcome::Failed), ("b", Outcome::Passed)]);
        assert_eq!(
            all_failures_quarantined(&one, &q(&["a"]), &|_| None),
            Some(vec!["a".to_owned()])
        );
        let two = outcomes(&[("a", Outcome::Failed), ("b", Outcome::Failed)]);
        assert_eq!(all_failures_quarantined(&two, &q(&["a"]), &|_| None), None);
        assert_eq!(all_failures_quarantined(&one, &q(&[]), &|_| None), None);
        let parent = outcomes(&[
            ("TestQ", Outcome::Failed),
            ("TestQ/new", Outcome::Failed),
            ("TestO", Outcome::Passed),
        ]);
        assert_eq!(
            all_failures_quarantined(&parent, &q(&["TestQ"]), &|_| Some(0)),
            None,
            "a subtest first seen at the quarantine is news"
        );
        let mut known = q(&["TestQ"]);
        known.get_mut("TestQ").unwrap().created_ms = 10;
        assert_eq!(
            all_failures_quarantined(&parent, &known, &|_| Some(5)),
            Some(vec!["TestQ".to_owned(), "TestQ/new".to_owned()])
        );
        let child = outcomes(&[
            ("TestP", Outcome::Failed),
            ("TestP/flaky", Outcome::Failed),
            ("TestO", Outcome::Passed),
        ]);
        assert_eq!(
            all_failures_quarantined(&child, &q(&["TestP/flaky"]), &|_| None),
            Some(vec!["TestP".to_owned(), "TestP/flaky".to_owned()])
        );
        let other = outcomes(&[
            ("TestP", Outcome::Failed),
            ("TestP/real", Outcome::Failed),
            ("TestP/flaky", Outcome::Failed),
        ]);
        assert_eq!(
            all_failures_quarantined(&other, &q(&["TestP/flaky"]), &|_| None),
            None
        );
    }
}

#[cfg(test)]
mod narrow_tests {
    /// Round 17 (go-make): `go test -v ./...` over several packages prints
    /// nothing until each package ends; a stall with no test event is that,
    /// not an opaque run.
    #[test]
    fn go_text_output_stalled_with_no_tests_says_go_buffers_per_package() {
        let n = super::go_buffering_note("make test");
        assert!(n.is_none());
        let n = super::go_buffering_note("go test -v ./...").unwrap_or_default();
        assert!(n.contains("buffers") && n.contains("go test -json"), "{n}");
        assert!(super::go_buffering_note("go test -json ./...").is_none());
        assert!(super::go_buffering_note("go test -v ./pkg").is_none());
    }

    /// Round 13 (Java): the hang note listed the last run's tests in
    /// alphabetical order, none of them in the class Surefire was running.
    #[test]
    fn a_running_class_narrows_the_suspects() {
        let missing: Vec<String> = [
            "calc.CalcParamTest::divides",
            "calc.CalcTest::hangs",
            "calc.CalcTest::adds",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let lines = vec![
            "[INFO] Running calc.CalcParamTest".to_owned(),
            "[INFO] Running calc.CalcTest".to_owned(),
        ];
        let (s, c) = super::narrow_by_running(&missing, &lines);
        assert_eq!(c.as_deref(), Some("calc.CalcTest"));
        assert_eq!(s, vec!["calc.CalcTest::hangs", "calc.CalcTest::adds"]);
        let (s, c) = super::narrow_by_running(&missing, &["building".to_owned()]);
        assert_eq!((s.len(), c), (3, None));
    }
}

#[cfg(test)]
mod readable_tests {
    /// Round 13 (Go): the excerpt under a failure was raw test2json.
    #[test]
    fn go_json_lines_read_as_their_output() {
        let lines = vec![
            r#"{"Action":"run","Test":"TestA"}"#.to_owned(),
            r#"{"Action":"output","Test":"TestA","Output":"    a_test.go:9: boom\n"}"#.to_owned(),
            "plain line".to_owned(),
        ];
        assert_eq!(
            super::readable(lines),
            vec!["    a_test.go:9: boom".to_owned(), "plain line".to_owned()]
        );
    }

    #[test]
    fn compiler_diagnostics_are_told_from_prose() {
        for l in [
            "vetbad.go:6:57: fmt.Sprintf format %s has arg n of wrong type int",
            "./app/core.py:3:1: F401 'os' imported but unused",
            "src/lib.rs:12:5: warning: unused",
            "app.ts:3: error: bad",
        ] {
            assert!(super::code_diagnostic(l), "{l}");
        }
        for l in [
            "make: *** [Makefile:3: ci] Error 1",
            "Total time: 3.2s",
            "12:30: started",
            "see https://example.com:8080/x",
            "",
        ] {
            assert!(!super::code_diagnostic(l), "{l}");
        }
    }

    #[test]
    fn makes_stop_points_at_the_step_above_it() {
        let tail: std::collections::VecDeque<String> = [
            "lint: TODO markers are not allowed in src/main",
            "",
            "make: *** [Makefile:3: ci] Error 1",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        assert_eq!(
            super::line_before_make_stop(&tail, "make: *** [Makefile:3: ci] Error 1"),
            Some("lint: TODO markers are not allowed in src/main".to_owned())
        );
        assert_eq!(super::line_before_make_stop(&tail, "error: boom"), None);
    }

    #[test]
    fn mavens_wrap_up_of_a_failed_test_step_is_the_runners() {
        assert!(super::runner_failure_summary("[INFO] BUILD FAILURE"));
        assert!(!super::runner_failure_summary("[INFO] BUILD SUCCESS"));
    }
}

#[cfg(test)]
mod selection_tests {
    use super::{in_selection, names_the_selection};

    /// Round 17: pytest prints a node id (`tests/test_calc.py::test_ok`);
    /// prova names the test `tests.test_calc::test_ok`.
    #[test]
    fn a_pytest_node_id_names_the_test() {
        let d = "tests.test_calc::test_ok";
        assert!(names_the_selection(d, "tests/test_calc.py::test_ok"));
        assert!(names_the_selection(d, "test_calc.py::test_ok"));
        assert!(names_the_selection(d, "tests.test_calc::test_ok"));
        assert!(!names_the_selection(d, "tests/test_other.py::test_ok"));
        let p = "tests.test_calc::test_param[a b-c::d]";
        assert!(names_the_selection(
            p,
            "tests/test_calc.py::test_param[a b-c::d]"
        ));
        assert!(names_the_selection(p, "tests/test_calc.py::test_param"));
        assert!(!names_the_selection(d, "tests/test_calc.py::test_o"));
    }

    /// Round 17: `TestTable/x` with no such subtest ran only the parent.
    #[test]
    fn a_parent_that_ran_is_not_its_missing_subtest() {
        assert!(!names_the_selection(
            "example.com/m::TestTable",
            "example.com/m::TestTable/x"
        ));
        assert!(names_the_selection(
            "example.com/m::TestTable/x",
            "example.com/m::TestTable/x"
        ));
        // Go writes spaces as underscores.
        assert!(names_the_selection("m::TestTable/x_y", "m::TestTable/x y"));
        assert!(names_the_selection(
            "m::TestTable/Name=1,_b=2",
            "m::TestTable/Name=1, b=2"
        ));
        assert!(names_the_selection(
            "m::TestTable/\u{e9}_\u{fc}",
            "m::TestTable/\u{e9} \u{fc}"
        ));
    }

    /// Round 17: tests "outside the selection" were tests the filter named.
    #[test]
    fn a_test_the_filter_names_is_inside_the_selection() {
        // pytest file filter.
        assert!(in_selection(
            "tests.test_fixtures::test_a",
            "test_fixtures.py"
        ));
        assert!(in_selection(
            "tests.test_fixtures::test_a",
            "tests/test_fixtures.py"
        ));
        assert!(!in_selection(
            "tests.test_other::test_a",
            "test_fixtures.py"
        ));
        // Go renames spaces.
        assert!(in_selection("m::TestTable/x_y", "TestTable/x y"));
        assert!(in_selection("m::TestTable/x_y", "m::TestTable/x y"));
        // Vitest nested names and a parent of the selection.
        assert!(in_selection(
            "src/a.test.js::math > ops > adds",
            "src/a.test.js::math > ops > adds"
        ));
        assert!(!in_selection(
            "src/a.test.js::math > ops > subs",
            "src/a.test.js::math > ops > adds"
        ));
        assert!(in_selection("m::TestTable", "m::TestTable/x"));
    }

    /// Issue #89: busy over the window, but the CPU was used earlier in it.
    #[test]
    fn busy_credits_the_last_cpu_use_not_now() {
        use std::collections::VecDeque;
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        let mut w: VecDeque<(Instant, u64)> = VecDeque::new();
        w.push_back((at(500), 30));
        w.push_back((at(1000), 0));
        w.push_back((at(1500), 0));
        w.push_back((at(4000), 0));
        assert_eq!(super::busy_since(&w), Some(at(500)));
        // A few stray ticks are no activity (an idle runtime).
        let mut idle: VecDeque<(Instant, u64)> = VecDeque::new();
        idle.push_back((at(500), 1));
        idle.push_back((at(1000), 1));
        assert_eq!(super::busy_since(&idle), None);
    }
}
