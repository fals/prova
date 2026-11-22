//! `toe` command line (ADR 0009). Primary conduit for agents and humans.

mod render;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand, ValueEnum};
use toe_core::analysis;
use toe_core::config::{self, CommandSpec, DEFAULT_KILL_AFTER, DEFAULT_STALL_AFTER, RunSpec};
use toe_core::engine::{self, NewRunRequest, Summary, Until, WaitReason};
use toe_core::ids::{now_ms, parse_duration};
use toe_core::model::{Outcome, Status};
use toe_core::store::Store;

/// Exit codes are a contract (ADR 0004). 2 is reserved for usage errors.
pub mod exit {
    pub const PASSED: u8 = 0;
    pub const FAILED: u8 = 1;
    pub const USAGE: u8 = 2;
    pub const STALLED_OR_KILLED: u8 = 3;
    pub const INFRA: u8 = 4;
    pub const RUNNING: u8 = 5;
}

#[derive(Parser)]
#[command(
    name = "toe",
    version,
    about = "Test oriented experimentation: structured test runs with memory"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum UntilArg {
    FirstFailure,
    Stall,
    Done,
}

impl From<UntilArg> for Until {
    fn from(u: UntilArg) -> Until {
        match u {
            UntilArg::FirstFailure => Until::FirstFailure,
            UntilArg::Stall => Until::Stall,
            UntilArg::Done => Until::Done,
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Run a suite from toe.toml, or any command after `--`.
    Run {
        /// Suite name from toe.toml.
        suite: Option<String>,
        /// Run only tests matching this filter (uses the suite's `select`).
        #[arg(long)]
        select: Option<String>,
        /// Return the run id immediately; follow with `toe wait`.
        #[arg(long)]
        detach: bool,
        /// Report a stall after this long without activity (e.g. 30s).
        #[arg(long)]
        stall_after: Option<String>,
        /// Kill the run after this long in total (e.g. 30m).
        #[arg(long)]
        kill_after: Option<String>,
        #[arg(long)]
        json: bool,
        /// Command to run, after `--`.
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Wait for the next significant event of a run, at most --max-wait.
    Wait {
        /// Run id, unique prefix, or `last`.
        run: String,
        #[arg(long, value_enum, default_value = "first-failure")]
        until: UntilArg,
        #[arg(long, default_value = "30s")]
        max_wait: String,
        /// Only consider events after this sequence number.
        #[arg(long, default_value_t = 0)]
        after: u64,
        #[arg(long)]
        json: bool,
    },
    /// Cancel a run (process group: SIGTERM, grace, SIGKILL).
    Cancel { run: String },
    /// Show one run, or recent runs.
    Status {
        run: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Print a run's events as JSON Lines.
    Events {
        run: String,
        #[arg(long, default_value_t = 0)]
        after: u64,
        /// Keep printing new events until the run ends.
        #[arg(long)]
        follow: bool,
    },
    /// Print a run's captured output.
    Output {
        run: String,
        #[arg(long)]
        tail: Option<usize>,
    },
    /// History and flake statistics of tests matching a name.
    History {
        /// Substring of the test's full name.
        test: String,
        #[arg(long)]
        suite: Option<String>,
        #[arg(long, default_value_t = 50)]
        window: usize,
        #[arg(long)]
        json: bool,
    },
    /// Tests with flaky evidence: passed on retry, or both passed and failed on the same code.
    Flaky {
        #[arg(long)]
        suite: Option<String>,
        #[arg(long, default_value_t = 100)]
        window: usize,
        #[arg(long)]
        json: bool,
    },
    /// Compare per-test results of two runs.
    Diff {
        base: String,
        head: String,
        #[arg(long)]
        json: bool,
    },
    #[command(name = "__supervise", hide = true)]
    Supervise {
        #[arg(long)]
        root: PathBuf,
        run: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(cli.cmd) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("toe: error: {e:#}");
            ExitCode::from(exit::INFRA)
        }
    }
}

fn open_here() -> Result<(Store, PathBuf, PathBuf)> {
    let cwd = std::env::current_dir()?;
    let root = config::find_root(&cwd);
    let store =
        Store::open(&root).with_context(|| format!("opening store in {}", root.display()))?;
    Ok((store, root, cwd))
}

fn duration_arg(v: &Option<String>, name: &str) -> Result<Option<u64>> {
    v.as_ref()
        .map(|s| {
            parse_duration(s)
                .map(|d| d.as_millis() as u64)
                .ok_or_else(|| anyhow!("invalid --{name} {s:?}; use e.g. 500ms, 30s, 10m"))
        })
        .transpose()
}

fn dispatch(cmd: Cmd) -> Result<u8> {
    match cmd {
        Cmd::Run {
            suite,
            select,
            detach,
            stall_after,
            kill_after,
            json,
            command,
        } => {
            let (store, root, cwd) = open_here()?;
            let cfg = config::load(&root)?;
            let (suite_name, mut spec, run_cwd) = if !command.is_empty() {
                if suite.is_some() || select.is_some() {
                    bail!("give either a suite name or a command after --, not both");
                }
                let rel = cwd
                    .strip_prefix(&root)
                    .unwrap_or(Path::new("."))
                    .to_string_lossy()
                    .into_owned();
                let rel = if rel.is_empty() { ".".to_owned() } else { rel };
                let spec = RunSpec {
                    command: CommandSpec::Argv(command.clone()),
                    size: None,
                    selection: None,
                    stall_after_ms: DEFAULT_STALL_AFTER.as_millis() as u64,
                    kill_after_ms: DEFAULT_KILL_AFTER.as_millis() as u64,
                    results: Vec::new(),
                    adapter: None,
                };
                (config::implicit_suite(&command, &rel), spec, cwd)
            } else {
                let name = match suite {
                    Some(s) => s,
                    None if cfg.suite.contains_key("default") => "default".into(),
                    None if cfg.suite.len() == 1 => {
                        cfg.suite.keys().next().cloned().unwrap_or_default()
                    }
                    None => bail!(
                        "nothing to run: give a command (`toe run -- make test`) or a suite name{}",
                        if cfg.suite.is_empty() {
                            String::new()
                        } else {
                            format!(
                                " ({})",
                                cfg.suite.keys().cloned().collect::<Vec<_>>().join(", ")
                            )
                        }
                    ),
                };
                let spec = cfg.run_spec(&name, select.as_deref())?;
                (name, spec, root.clone())
            };
            if let Some(ms) = duration_arg(&stall_after, "stall-after")? {
                spec.stall_after_ms = ms;
            }
            if let Some(ms) = duration_arg(&kill_after, "kill-after")? {
                spec.kill_after_ms = ms;
            }
            let id = engine::create_run(
                &store,
                &NewRunRequest {
                    suite: suite_name,
                    spec,
                    cwd: run_cwd,
                    extra_env: cfg.env.clone(),
                },
            )?;
            let exe = std::env::current_exe()?;
            let mut child =
                engine::spawn_supervisor(&store, &exe, &id).context("starting supervisor")?;
            if detach {
                if json {
                    println!("{}", serde_json::json!({ "run": id }));
                } else {
                    println!("run {id} started");
                    println!("next: toe wait {id}");
                }
                return Ok(exit::PASSED);
            }
            let code = attached(&store, &id, json);
            let _ = child.wait();
            code
        }
        Cmd::Wait {
            run,
            until,
            max_wait,
            after,
            json,
        } => {
            let (store, _, _) = open_here()?;
            let id = store.resolve_run(&run)?;
            let max = parse_duration(&max_wait)
                .ok_or_else(|| anyhow!("invalid --max-wait {max_wait:?}"))?;
            let s = engine::wait(&store, &id, until.into(), after, max)?;
            render::summary(&s, json);
            Ok(exit_code(&s))
        }
        Cmd::Cancel { run } => {
            let (store, _, _) = open_here()?;
            let id = store.resolve_run(&run)?;
            if store.cancel_created(&id, now_ms())? {
                println!("run {id} cancelled before start");
                return Ok(exit::PASSED);
            }
            let row = store.run(&id)?;
            if row.status.is_terminal() {
                println!("run {id} already {}", row.status.as_str());
                return Ok(exit::PASSED);
            }
            store.request_cancel(&id)?;
            println!("cancel requested for run {id}");
            println!("next: toe wait {id} --until done");
            Ok(exit::PASSED)
        }
        Cmd::Status { run, json } => {
            let (store, _, _) = open_here()?;
            match run {
                Some(r) => {
                    let id = store.resolve_run(&r)?;
                    let seq = store.last_seq(&id)?;
                    let row = store.run(&id)?;
                    let reason = if row.status.is_terminal() {
                        WaitReason::Done
                    } else {
                        WaitReason::Running
                    };
                    let s = engine::summarize(&store, &id, reason, seq)?;
                    render::summary(&s, json);
                    Ok(exit_code(&s))
                }
                None => {
                    render::runs(&store.recent_runs(20)?, json);
                    Ok(exit::PASSED)
                }
            }
        }
        Cmd::Events { run, after, follow } => {
            let (store, _, _) = open_here()?;
            let id = store.resolve_run(&run)?;
            let mut cursor = after;
            loop {
                let events = store.events_after(&id, cursor)?;
                let mut ended = false;
                for e in &events {
                    cursor = e.seq;
                    println!("{}", serde_json::to_string(e)?);
                    ended |= matches!(e.kind, toe_core::model::EventKind::RunFinished { .. });
                }
                if !follow || ended {
                    return Ok(exit::PASSED);
                }
                // Reuse wait's orphan detection and bounded blocking.
                let _ = engine::wait(&store, &id, Until::Done, cursor, Duration::from_millis(500))?;
            }
        }
        Cmd::Output { run, tail } => {
            let (store, _, _) = open_here()?;
            let id = store.resolve_run(&run)?;
            match tail {
                Some(n) => engine::output_tail(&store, &id, n)
                    .iter()
                    .for_each(|l| println!("{l}")),
                None => {
                    let path = store.run_dir(&id).join("output.log");
                    let mut f = std::fs::File::open(&path)
                        .with_context(|| format!("{}", path.display()))?;
                    std::io::copy(&mut f, &mut std::io::stdout())?;
                }
            }
            Ok(exit::PASSED)
        }
        Cmd::History {
            test,
            suite,
            window,
            json,
        } => {
            let (store, _, _) = open_here()?;
            let found = analysis::find_tests(&store, &test, suite.as_deref())?;
            let exact: Vec<_> = found.iter().filter(|t| t.1 == test).collect();
            // Same name in several suites: take the one that ran most recently.
            let displays: std::collections::BTreeSet<&str> =
                found.iter().map(|t| t.1.as_str()).collect();
            let pick = match (exact.as_slice(), found.as_slice()) {
                ([one], _) => one.0.clone(),
                ([], [one]) => one.0.clone(),
                (_, [_, ..]) if displays.len() == 1 => analysis::most_recent(&store, &found)?,
                (_, []) => {
                    bail!("no test matching {test:?}; tests are recorded from level 1 runs on")
                }
                _ => {
                    println!("{} tests match {test:?}; be more specific:", found.len());
                    for (_, display, suite) in found.iter().take(20) {
                        println!("  {display}  (suite {suite})");
                    }
                    return Ok(exit::USAGE);
                }
            };
            match analysis::stats(&store, &pick, window)? {
                Some(s) => render::test_stats(&s, json),
                None => bail!("test disappeared"),
            }
            Ok(exit::PASSED)
        }
        Cmd::Flaky {
            suite,
            window,
            json,
        } => {
            let (store, _, _) = open_here()?;
            let list = analysis::flaky(&store, suite.as_deref(), window)?;
            if json {
                println!("{}", serde_json::to_string(&list)?);
            } else if list.is_empty() {
                println!("no flaky tests in recorded history");
            } else {
                for s in &list {
                    println!(
                        "{:5.1}% fail  {:2} retry-passes  {:2} same-code flips  {}",
                        s.failure_rate * 100.0,
                        s.passed_on_retry,
                        s.flips_on_same_code,
                        render::clean(&s.display)
                    );
                }
            }
            Ok(exit::PASSED)
        }
        Cmd::Diff { base, head, json } => {
            let (store, _, _) = open_here()?;
            let d = analysis::diff(
                &store,
                &store.resolve_run(&base)?,
                &store.resolve_run(&head)?,
            )?;
            if json {
                println!("{}", serde_json::to_string(&d)?);
            } else {
                println!("diff {} -> {}", d.base, d.head);
                for (label, v) in [
                    ("newly failing", &d.newly_failing),
                    ("fixed", &d.fixed),
                    ("still failing", &d.still_failing),
                    ("added", &d.added),
                    ("removed", &d.removed),
                ] {
                    if !v.is_empty() {
                        println!("{label} ({}):", v.len());
                        v.iter()
                            .take(30)
                            .for_each(|t| println!("  {}", render::clean(t)));
                    }
                }
                for (t, a, b) in &d.slower {
                    println!("slower: {} {a}ms -> {b}ms", render::clean(t));
                }
            }
            Ok(if d.newly_failing.is_empty() {
                exit::PASSED
            } else {
                exit::FAILED
            })
        }
        Cmd::Supervise { root, run } => {
            let store = Store::open(&root)?;
            let row = store.run(&run)?;
            let spec: RunSpec = serde_json::from_value(row.spec)?;
            let mut observer = toe_adapters::observer_for(&spec, &root);
            engine::supervise(&store, &run, observer.as_mut())?;
            Ok(exit::PASSED)
        }
    }
}

/// Attached run: wait loops until the end, reporting significant events.
fn attached(store: &Store, id: &str, json: bool) -> Result<u8> {
    let interrupted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let flag = interrupted.clone();
        let _ = ctrlc::set_handler(move || flag.store(true, std::sync::atomic::Ordering::SeqCst));
    }
    if !json {
        eprintln!("run {id} started (Ctrl-C cancels)");
    }
    let mut cursor = 0;
    let mut cancel_sent = false;
    let mut reported: std::collections::BTreeSet<String> = Default::default();
    loop {
        if interrupted.load(std::sync::atomic::Ordering::SeqCst) && !cancel_sent {
            cancel_sent = true;
            if !store.cancel_created(id, now_ms())? {
                store.request_cancel(id)?;
            }
        }
        let s = engine::wait(
            store,
            id,
            Until::FirstFailure,
            cursor,
            Duration::from_millis(500),
        )?;
        cursor = s.last_seq;
        match s.reason {
            WaitReason::Done => {
                render::summary(&s, json);
                return Ok(exit_code(&s));
            }
            WaitReason::Failure | WaitReason::Stall if !json => render::progress(&s, &mut reported),
            _ => {}
        }
    }
}

fn exit_code(s: &Summary) -> u8 {
    match s.reason {
        WaitReason::Running => exit::RUNNING,
        WaitReason::Stall => exit::STALLED_OR_KILLED,
        WaitReason::Failure => exit::FAILED,
        WaitReason::Done => match (s.outcome, s.status) {
            (Some(Outcome::Passed), _) => exit::PASSED,
            (Some(Outcome::Failed), _) => exit::FAILED,
            (_, Status::Cancelled) => exit::STALLED_OR_KILLED,
            (_, Status::Aborted)
                if s.detail.as_deref().is_some_and(|d| d.starts_with("killed")) =>
            {
                exit::STALLED_OR_KILLED
            }
            (_, Status::Finished) => exit::FAILED,
            _ => exit::INFRA,
        },
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn usage_exit_code_matches_clap() {
        assert_eq!(super::exit::USAGE, 2);
    }
}
