//! `prova` command line (ADR 0009). Primary conduit for agents and humans.

mod mcp;
mod render;
mod skills;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand, ValueEnum};
use prova_core::analysis;
use prova_core::config::{self, CommandSpec, DEFAULT_KILL_AFTER, DEFAULT_STALL_AFTER, RunSpec};
use prova_core::engine::{self, NewRunRequest, Summary, Until, WaitReason};
use prova_core::experiment;
use prova_core::ids::{now_ms, parse_duration};
use prova_core::model::{Outcome, Status};
use prova_core::store::Store;

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
    name = "prova",
    version,
    about = "prova: tests with memory, experiments with proof"
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
    /// Run a suite from prova.toml, or any command after `--`.
    Run {
        /// Suite name from prova.toml.
        suite: Option<String>,
        /// Run only tests matching this filter (uses the suite's `select`).
        #[arg(long)]
        select: Option<String>,
        /// Return the run id immediately; follow with `prova wait`.
        #[arg(long, conflicts_with = "until")]
        detach: bool,
        /// Start the run and wait for this in one command (ADR 0019):
        /// `done` (the result), `first-failure`, or `stall`.
        #[arg(long, value_enum)]
        until: Option<UntilArg>,
        /// With --until: wait at most this long (default 9m for `done`,
        /// 30s otherwise); then exit 5 with the command to continue.
        #[arg(long, requires = "until")]
        max_wait: Option<String>,
        /// Report a stall after this long without activity (e.g. 30s).
        /// Default: the suite's `stall_after`, else by size (small 30s,
        /// medium 60s, large 5m), else 60s.
        #[arg(long)]
        stall_after: Option<String>,
        /// Kill the run after this long in total (e.g. 30m).
        #[arg(long)]
        kill_after: Option<String>,
        /// Opt-in environment injection recipe (repeatable), e.g. pytest-junit.
        #[arg(long = "inject")]
        inject: Vec<String>,
        /// Keep the whole output (compressed), not only the excerpt: for
        /// debugging prova's recognizers.
        #[arg(long)]
        keep_output: bool,
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
        /// Default: 9m for `--until done` (under the usual 10-minute limit of
        /// one tool call), 30s otherwise (ADR 0019).
        #[arg(long)]
        max_wait: Option<String>,
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
        /// Recent runs of every repository known to this prova home.
        #[arg(long, conflicts_with = "run")]
        all: bool,
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
    /// Print a run's output: live while it runs; once it ended, the kept
    /// excerpt (head, error blocks, tail) and failing tests' own output.
    Output {
        run: String,
        /// Last lines only.
        #[arg(long)]
        tail: Option<usize>,
        /// One failing test's own output (substring of its name).
        #[arg(long)]
        test: Option<String>,
        #[arg(long)]
        json: bool,
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
        /// Across every repository known to this prova home.
        #[arg(long)]
        all: bool,
        #[arg(long, default_value_t = 100)]
        window: usize,
        #[arg(long)]
        json: bool,
    },
    /// Proposed test sizes from observed durations.
    Sizes {
        #[arg(long)]
        suite: Option<String>,
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
    /// Install prova skills for agents and add a short section to AGENTS.md.
    Init {
        /// Also install into .claude/skills (default: when .claude/ or CLAUDE.md exists).
        #[arg(long)]
        claude: bool,
    },
    /// Detect the test setup and propose prova.toml; verify single-test selection.
    Adopt {
        /// Write the proposal to prova.toml (only if none exists).
        #[arg(long)]
        write: bool,
        /// Verify the `select` template of this suite.
        #[arg(long, requires = "filter")]
        verify: Option<String>,
        /// A real test name to select when verifying.
        #[arg(long)]
        filter: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Serve the MCP facade on stdio (tools mirror the CLI).
    Mcp,
    /// Experiments: frozen protocol, interleaved trials, statistical verdict.
    #[command(subcommand)]
    Exp(ExpCmd),
    #[command(name = "__exp", hide = true)]
    ExpWorker {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        dir: PathBuf,
        comparison: String,
    },
    #[command(name = "__supervise", hide = true)]
    Supervise {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        dir: PathBuf,
        run: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum BetterArg {
    Lower,
    Higher,
}

impl From<BetterArg> for experiment::Better {
    fn from(b: BetterArg) -> Self {
        match b {
            BetterArg::Lower => experiment::Better::Lower,
            BetterArg::Higher => experiment::Better::Higher,
        }
    }
}

/// `prova exp protocol`: every option optional, so what is not given keeps
/// its value (round 5: defaults silently replaced the protocol and dropped
/// the oracle).
#[derive(clap::Args)]
struct ProtocolChange {
    #[arg(long)]
    metric: Option<String>,
    #[arg(long, value_enum)]
    better: Option<BetterArg>,
    #[arg(long)]
    unit: Option<String>,
    #[arg(long)]
    min_effect: Option<String>,
    #[arg(long)]
    alpha: Option<f64>,
    #[arg(long)]
    trials: Option<u32>,
    #[arg(long)]
    min_trials: Option<u32>,
    #[arg(long)]
    max_trials: Option<u32>,
    #[arg(long)]
    warmup: Option<u32>,
    /// Replaces the guardrails (NAME or NAME:higher), repeatable.
    #[arg(long = "guardrail")]
    guardrails: Vec<String>,
    #[arg(long)]
    kill_after: Option<String>,
    #[arg(long)]
    setup: Option<String>,
    /// Adds a specification file (repeatable).
    #[arg(long = "spec")]
    specs: Vec<PathBuf>,
    #[arg(long)]
    oracle: Option<String>,
}

#[derive(clap::Args)]
struct ProtocolArgs {
    /// Primary metric: a JSON line {"metric": NAME, "value": N} the workload
    /// prints, or one read from benchmark output (go test -bench, criterion,
    /// pytest-benchmark, Vitest, JMH, BenchmarkDotNet). `prova exp metrics -- <cmd>`
    /// lists them; `wall_ms` is always recorded.
    #[arg(long, default_value = "wall_ms")]
    metric: String,
    #[arg(long, value_enum, default_value = "lower")]
    better: BetterArg,
    #[arg(long)]
    unit: Option<String>,
    /// Smallest change that matters, e.g. 2% or 0.02.
    #[arg(long, default_value = "2%")]
    min_effect: String,
    #[arg(long, default_value_t = 0.05)]
    alpha: f64,
    /// Trials per arm before the first verdict.
    #[arg(long, default_value_t = 10)]
    trials: u32,
    #[arg(long, default_value_t = 5)]
    min_trials: u32,
    /// Inconclusive results extend up to this many trials per arm.
    #[arg(long, default_value_t = 20)]
    max_trials: u32,
    #[arg(long, default_value_t = 1)]
    warmup: u32,
    /// Metric that must not get worse: NAME or NAME:higher (higher is better).
    #[arg(long = "guardrail")]
    guardrails: Vec<String>,
    /// Kill a single trial after this long.
    #[arg(long, default_value = "10m")]
    kill_after: String,
    /// Shell command run once in each variant's directory before trials,
    /// e.g. "npm ci" (the baseline worktree has no installed dependencies).
    #[arg(long)]
    setup: Option<String>,
    /// Specification of the problem (Quint, Lean, TLA+, anything): frozen
    /// by content hash with the protocol. Repeatable. Commit it first.
    #[arg(long = "spec")]
    specs: Vec<PathBuf>,
    /// Command that must succeed for a variant to satisfy the specification
    /// (e.g. "quint run spec/x.qnt --invariant safety && make conformance").
    /// A violating candidate is rejected without being measured.
    #[arg(long)]
    oracle: Option<String>,
}

#[derive(Subcommand)]
enum ExpCmd {
    /// Create an experiment; the baseline is a git ref (default HEAD).
    New {
        name: String,
        #[arg(long)]
        hypothesis: String,
        /// Baseline git ref; `.` means the working tree as it is now.
        #[arg(long, default_value = "HEAD")]
        baseline: String,
        #[command(flatten)]
        protocol: ProtocolArgs,
        /// Workload command, after `--`.
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Change the protocol (refused after the first trial). Only the options
    /// given change; `--spec` adds files; the result is printed.
    Protocol {
        name: String,
        #[command(flatten)]
        change: ProtocolChange,
    },
    /// Record a candidate: the working tree now (default) or a git ref.
    Candidate {
        name: String,
        /// Candidate name (default c1, c2, ...).
        #[arg(long)]
        label: Option<String>,
        #[arg(long = "ref")]
        git_ref: Option<String>,
        /// Parameter passed to the workload as PROVA_PARAM_<KEY>.
        #[arg(long = "param")]
        params: Vec<String>,
    },
    /// Run trials of a candidate against the current best and decide.
    Run {
        name: String,
        /// Candidate (default: the latest recorded).
        #[arg(long)]
        candidate: Option<String>,
        /// Compare against this variant instead of the current best.
        #[arg(long)]
        against: Option<String>,
        #[arg(long)]
        detach: bool,
        #[arg(long)]
        json: bool,
    },
    /// Re-measure the current best against the original baseline.
    Revalidate {
        name: String,
        #[arg(long)]
        detach: bool,
        #[arg(long)]
        json: bool,
    },
    /// Wait for the running comparison, at most --max-wait.
    Wait {
        name: String,
        #[arg(long, default_value = "30s")]
        max_wait: String,
        #[arg(long)]
        json: bool,
    },
    /// Show the experiment, latest comparison and decision log.
    Status {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Cancel the running comparison.
    Cancel { name: String },
    /// List experiments.
    List,
    /// Kept logs of what failed in the latest comparison (a trial, the
    /// setup, the oracle).
    Log {
        name: String,
        /// One log: a trial number, setup-baseline, oracle-candidate, ...
        entry: Option<String>,
    },
    /// Run a workload once and show the metrics prova reads from it
    /// (benchmark tools and JSON metric lines), to choose --metric.
    Metrics {
        #[arg(long)]
        json: bool,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
}

fn parse_effect(s: &str) -> Result<f64> {
    let t = s.trim();
    let v = match t.strip_suffix('%') {
        Some(p) => p.trim().parse::<f64>().map(|v| v / 100.0),
        None => t.parse::<f64>(),
    };
    v.map_err(|_| anyhow!("invalid effect {s:?}; use e.g. 2% or 0.02"))
}

fn spec_files(files: &[PathBuf], root: &Path) -> Result<Vec<experiment::SpecFile>> {
    files
        .iter()
        .map(|f| {
            let abs = std::fs::canonicalize(f)
                .with_context(|| format!("specification {}", f.display()))?;
            let rel = abs
                .strip_prefix(std::fs::canonicalize(root)?)
                .map_err(|_| anyhow!("specification {} is outside the repository", f.display()))?;
            Ok(experiment::SpecFile {
                path: rel.to_string_lossy().into_owned(),
                sha256: prova_core::ids::sha256_hex(&std::fs::read(&abs)?),
            })
        })
        .collect()
}

fn parse_guardrails(list: &[String]) -> Result<Vec<experiment::Guardrail>> {
    list.iter()
        .map(|g| {
            let (m, better) = match g.split_once(':') {
                Some((m, "higher")) => (m, experiment::Better::Higher),
                Some((m, "lower")) => (m, experiment::Better::Lower),
                Some((_, other)) => {
                    bail!("guardrail direction must be higher or lower, got {other:?}")
                }
                None => (g.as_str(), experiment::Better::Lower),
            };
            Ok(experiment::Guardrail {
                metric: m.to_owned(),
                better,
            })
        })
        .collect()
}

fn protocol_from(
    a: &ProtocolArgs,
    command: Vec<String>,
    root: &Path,
) -> Result<experiment::Protocol> {
    let specs = spec_files(&a.specs, root)?;
    let guardrails = parse_guardrails(&a.guardrails)?;
    Ok(experiment::Protocol {
        metric: a.metric.clone(),
        unit: a.unit.clone(),
        better: a.better.into(),
        min_effect: parse_effect(&a.min_effect)?,
        alpha: a.alpha,
        trials: a.trials,
        min_trials: a.min_trials,
        max_trials: a.max_trials,
        warmup: a.warmup,
        guardrails,
        command,
        setup: a.setup.clone(),
        specs,
        oracle: a.oracle.clone(),
        kill_after_ms: parse_duration(&a.kill_after)
            .ok_or_else(|| anyhow!("invalid --kill-after"))?
            .as_millis() as u64,
    })
}

fn spawn_exp_worker(store: &Store, comparison: &str) -> Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    let dir = store.dir().join("experiments").join(comparison);
    prova_core::store::private_dir(&dir)?;
    let log = prova_core::store::private_file(&dir.join("worker.log"))?;
    Ok(std::process::Command::new(std::env::current_exe()?)
        .arg("__exp")
        .arg("--root")
        .arg(store.root())
        .arg("--dir")
        .arg(store.dir())
        .arg(comparison)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log)
        .process_group(0)
        .spawn()?)
}

fn exp_exit(r: &experiment::Report) -> u8 {
    match r
        .comparison
        .as_ref()
        .map(|c| (c.status.as_str(), c.verdict.as_deref()))
    {
        Some(("running", _)) => exit::RUNNING,
        Some(("decided", Some("accept"))) => exit::PASSED,
        Some(("decided", _)) => exit::FAILED,
        Some(("cancelled", _)) => exit::STALLED_OR_KILLED,
        Some(_) => exit::INFRA,
        None => exit::PASSED,
    }
}

fn exp_start(
    store: &Store,
    name: &str,
    candidate: &str,
    against: Option<&str>,
    detach: bool,
    json: bool,
) -> Result<u8> {
    let id = experiment::start(store, name, candidate, against)?;
    let best = experiment::get(store, name)?.best;
    let live = experiment::live_tree_variants(store, name, &[candidate, against.unwrap_or(&best)]);
    if !live.is_empty() {
        eprintln!(
            "warning: {} has no snapshot (recorded before snapshots, or outside git) and runs in the live working tree; record it again with `prova exp candidate {name}` to pin it",
            live.join(", ")
        );
    }
    let mut child = spawn_exp_worker(store, &id)?;
    if detach {
        if json {
            println!(
                "{}",
                serde_json::json!({ "comparison": id, "experiment": name })
            );
        } else {
            println!(
                "comparison {id} started: {candidate} vs {}",
                against.unwrap_or("best")
            );
            println!("next: prova exp wait {name}");
        }
        return Ok(exit::PASSED);
    }
    loop {
        let r = experiment::wait(store, name, Duration::from_secs(30))?;
        if exp_exit(&r) != exit::RUNNING {
            let _ = child.wait();
            render::experiment(&r, json);
            return Ok(exp_exit(&r));
        }
        if !json {
            render::exp_progress(&r);
        }
    }
}

fn write_skills(dir: &Path, names: &[String]) -> Result<usize> {
    let mut n = 0;
    for (skill, rel, content) in skills::FILES {
        if !names.iter().any(|s| s == skill) {
            continue;
        }
        let path = dir.join(skill).join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, content)?;
        n += 1;
    }
    Ok(n)
}

fn init(claude: bool) -> Result<u8> {
    let cwd = std::env::current_dir()?;
    let root = config::find_root(&cwd);
    let proposal = prova_adapters::adopt::propose(&root);
    let mut names: Vec<String> = skills::WORKFLOW.iter().map(|s| s.to_string()).collect();
    for lang in &proposal.languages {
        let skill = format!("prova-{lang}");
        if skills::FILES.iter().any(|f| f.0 == skill) {
            names.push(skill);
        }
    }
    let n = write_skills(&root.join(".agents/skills"), &names)?;
    println!(
        "installed {} skills ({n} files) into .agents/skills: {}",
        names.len(),
        names.join(", ")
    );
    if claude || root.join(".claude").is_dir() || root.join("CLAUDE.md").exists() {
        write_skills(&root.join(".claude/skills"), &names)?;
        println!("also installed into .claude/skills");
    }
    // AGENTS.md: replace our section between markers, or append it.
    let agents = root.join("AGENTS.md");
    let text = std::fs::read_to_string(&agents).unwrap_or_default();
    let new = match (
        text.find("<!-- prova:begin -->"),
        text.find("<!-- prova:end -->"),
    ) {
        (Some(a), Some(b)) if b > a => {
            let end = b + "<!-- prova:end -->".len();
            format!(
                "{}{}{}",
                &text[..a],
                skills::AGENTS_SECTION.trim_end(),
                &text[end..]
            )
        }
        _ if text.is_empty() => skills::AGENTS_SECTION.to_owned(),
        _ => format!("{}\n\n{}", text.trim_end(), skills::AGENTS_SECTION),
    };
    if new != text {
        std::fs::write(&agents, new)?;
        println!("updated AGENTS.md (prova section)");
    }
    let gi = root.join(".gitignore");
    let ignore = std::fs::read_to_string(&gi).unwrap_or_default();
    if !ignore
        .lines()
        .any(|l| l.trim() == ".prova/" || l.trim() == ".prova")
    {
        let sep = if ignore.is_empty() || ignore.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        std::fs::write(&gi, format!("{ignore}{sep}.prova/\n"))?;
        println!("added .prova/ to .gitignore");
    }
    if !root.join("prova.toml").exists() {
        println!("next: prova adopt   (proposes prova.toml for this project)");
    }
    Ok(exit::PASSED)
}

/// Runs a spec to the end, without detaching, and returns its summary.
fn run_to_end(store: &Store, req: &NewRunRequest) -> Result<Summary> {
    let id = engine::create_run(store, req)?;
    let mut child = engine::spawn_supervisor(store, &std::env::current_exe()?, &id)?;
    let mut cursor = 0;
    loop {
        let s = engine::wait(store, &id, Until::Done, cursor, Duration::from_secs(30))?;
        cursor = s.last_seq;
        if s.reason == WaitReason::Done {
            let _ = child.wait();
            return Ok(s);
        }
    }
}

fn adopt(write: bool, verify: Option<String>, filter: Option<String>, json: bool) -> Result<u8> {
    let (store, root, _) = open_here()?;
    if let (Some(suite), Some(filter)) = (verify, filter) {
        let cfg = config::load(&root)?;
        let spec = cfg.run_spec(&suite, Some(&filter))?;
        prova_adapters::inject::validate(&spec.inject).map_err(|e| anyhow!(e))?;
        let s = run_to_end(
            &store,
            &NewRunRequest {
                suite: suite.clone(),
                spec,
                cwd: root.clone(),
                extra_env: cfg.env.clone(),
            },
        )?;
        let tests: Vec<String> = store
            .events_after(&s.run, 0)?
            .into_iter()
            .filter_map(|e| match e.kind {
                prova_core::model::EventKind::TestFinished { test, .. } => Some(test.display()),
                _ => None,
            })
            .collect();
        let unrelated: Vec<&String> = tests.iter().filter(|t| !t.contains(&filter)).collect();
        let (ok, why) = if s.level == prova_core::model::Level::Opaque {
            (false, "cannot verify: run is at level opaque (no per-test results); see the prova-<language> skill to raise the level".to_owned())
        } else if tests.is_empty() {
            (
                false,
                format!(
                    "no test ran for filter {filter:?}; the select template does not reach the test"
                ),
            )
        } else if !unrelated.is_empty() {
            (
                false,
                format!(
                    "{} tests ran that do not match {filter:?} (e.g. {}); select is not selective",
                    unrelated.len(),
                    unrelated[0]
                ),
            )
        } else {
            (
                true,
                format!("{} matching test(s) ran and nothing else", tests.len()),
            )
        };
        if json {
            println!(
                "{}",
                serde_json::json!({ "run": s.run, "verified": ok, "reason": why, "tests": tests })
            );
        } else {
            println!(
                "{} run {}: {why}",
                if ok { "VERIFIED" } else { "NOT VERIFIED" },
                s.run
            );
        }
        return Ok(if ok { exit::PASSED } else { exit::FAILED });
    }
    let p = prova_adapters::adopt::propose(&root);
    let toml = prova_adapters::adopt::to_toml(&p);
    if json {
        println!("{}", serde_json::json!({ "proposal": p, "toml": toml }));
    } else if !write {
        print!("{toml}");
    }
    if write {
        let path = root.join("prova.toml");
        if path.exists() {
            bail!("prova.toml exists; edit it by hand or remove it first");
        }
        if p.suites.is_empty() {
            bail!("nothing to write");
        }
        std::fs::write(&path, &toml)?;
        if !json {
            let names: Vec<&str> = p.suites.iter().map(|s| s.name.as_str()).collect();
            println!("wrote prova.toml with suite(s) {}", names.join(", "));
            for n in &p.notes {
                println!("note: {n}");
            }
            println!(
                "next: prova adopt --verify {} --filter <test name>",
                names[0]
            );
        }
    }
    Ok(exit::PASSED)
}

fn exp(cmd: ExpCmd) -> Result<u8> {
    let (store, root, cwd) = open_here()?;
    match cmd {
        ExpCmd::New {
            name,
            hypothesis,
            baseline,
            protocol,
            command,
        } => {
            let p = protocol_from(&protocol, command, &root)?;
            let base = if baseline == "." {
                None
            } else {
                Some(baseline.as_str())
            };
            experiment::create(&store, &name, &hypothesis, &p, &cwd, base)?;
            println!(
                "experiment {name} created (protocol {}), baseline = {baseline}",
                p.hash()
            );
            println!(
                "next: change the code, then `prova exp candidate {name}` and `prova exp run {name}`"
            );
            Ok(exit::PASSED)
        }
        ExpCmd::Protocol { name, change: c } => {
            let mut p = experiment::get(&store, &name)?.protocol;
            if let Some(v) = c.metric {
                p.metric = v;
            }
            if let Some(v) = c.better {
                p.better = v.into();
            }
            if c.unit.is_some() {
                p.unit = c.unit;
            }
            if let Some(v) = c.min_effect {
                p.min_effect = parse_effect(&v)?;
            }
            if let Some(v) = c.alpha {
                p.alpha = v;
            }
            if let Some(v) = c.trials {
                p.trials = v;
            }
            if let Some(v) = c.min_trials {
                p.min_trials = v;
            }
            if let Some(v) = c.max_trials {
                p.max_trials = v;
            }
            if let Some(v) = c.warmup {
                p.warmup = v;
            }
            if !c.guardrails.is_empty() {
                p.guardrails = parse_guardrails(&c.guardrails)?;
            }
            if let Some(v) = c.kill_after {
                p.kill_after_ms = parse_duration(&v)
                    .ok_or_else(|| anyhow!("invalid --kill-after"))?
                    .as_millis() as u64;
            }
            if c.setup.is_some() {
                p.setup = c.setup;
            }
            for f in spec_files(&c.specs, &root)? {
                p.specs.retain(|s| s.path != f.path);
                p.specs.push(f);
            }
            if c.oracle.is_some() {
                p.oracle = c.oracle;
            }
            experiment::set_protocol(&store, &name, &p)?;
            println!("protocol of {name} updated (hash {})", p.hash());
            println!("{}", render::protocol_line(&p));
            Ok(exit::PASSED)
        }
        ExpCmd::Candidate {
            name,
            label,
            git_ref,
            params,
        } => {
            experiment::get(&store, &name)?;
            let label = match label {
                Some(l) => l,
                None => format!("c{}", experiment::variants(&store, &name)?.len()),
            };
            let mut map = std::collections::BTreeMap::new();
            for p in params {
                let (k, v) = p
                    .split_once('=')
                    .ok_or_else(|| anyhow!("--param needs KEY=VALUE, got {p:?}"))?;
                map.insert(k.to_owned(), v.to_owned());
            }
            experiment::add_variant(&store, &name, &label, git_ref.as_deref(), &map)?;
            let v = experiment::variant(&store, &name, &label)?;
            println!("candidate {label} recorded at {}", v.code);
            println!("next: prova exp run {name}");
            Ok(exit::PASSED)
        }
        ExpCmd::Run {
            name,
            candidate,
            against,
            detach,
            json,
        } => {
            let candidate = match candidate {
                Some(c) => c,
                None => experiment::variants(&store, &name)?
                    .last()
                    .filter(|v| v.name != "baseline")
                    .map(|v| v.name.clone())
                    .ok_or_else(|| anyhow!("no candidate yet: prova exp candidate {name}"))?,
            };
            exp_start(&store, &name, &candidate, against.as_deref(), detach, json)
        }
        ExpCmd::Revalidate { name, detach, json } => {
            let e = experiment::get(&store, &name)?;
            if e.best == "baseline" {
                bail!("nothing to revalidate: no candidate has been kept");
            }
            exp_start(&store, &name, &e.best, Some("baseline"), detach, json)
        }
        ExpCmd::Wait {
            name,
            max_wait,
            json,
        } => {
            let max = parse_duration(&max_wait).ok_or_else(|| anyhow!("invalid --max-wait"))?;
            let r = experiment::wait(&store, &name, max)?;
            render::experiment(&r, json);
            Ok(exp_exit(&r))
        }
        ExpCmd::Status { name, json } => {
            let r = experiment::report(&store, &name)?;
            render::experiment(&r, json);
            Ok(exp_exit(&r))
        }
        ExpCmd::Cancel { name } => {
            let id = experiment::latest_comparison(&store, &name)?
                .ok_or_else(|| anyhow!("no comparison to cancel"))?;
            experiment::request_cancel(&store, &id)?;
            println!("cancel requested for comparison {id}; it stops after the current trial");
            Ok(exit::PASSED)
        }
        ExpCmd::Log { name, entry } => {
            let Some(c) = experiment::latest_comparison(&store, &name)? else {
                bail!("experiment {name} has no comparison yet");
            };
            let kept = prova_core::output::comparison_excerpts(&store, &c)?;
            let shown: Vec<_> = kept
                .iter()
                .filter(|(n, _)| entry.as_deref().is_none_or(|e| e == n))
                .collect();
            if shown.is_empty() {
                println!("nothing kept for comparison {c}: logs are kept only for what failed");
            }
            for (n, e) in shown {
                println!("== {n}");
                print!("{}", e.render());
            }
            Ok(exit::PASSED)
        }
        ExpCmd::Metrics { json, command } => {
            let start = std::time::Instant::now();
            let out = std::process::Command::new(&command[0])
                .args(&command[1..])
                .stdin(std::process::Stdio::null())
                .output()
                .with_context(|| format!("running {}", command.join(" ")))?;
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            let mut metrics = prova_adapters::bench::parse(&text);
            metrics.push((
                "wall_ms".into(),
                start.elapsed().as_secs_f64() * 1000.0,
                Some("ms".into()),
            ));
            if json {
                let v: Vec<_> = metrics
                    .iter()
                    .map(|(n, v, u)| serde_json::json!({ "metric": n, "value": v, "unit": u }))
                    .collect();
                println!("{}", serde_json::to_string(&v)?);
            } else {
                for (n, v, u) in &metrics {
                    println!("{n:40} {v:>16.3} {}", u.as_deref().unwrap_or(""));
                }
                println!(
                    "next: prova exp new <name> --metric <one of these> --better lower|higher -- {}",
                    sh_quote(&command)
                );
            }
            Ok(if out.status.success() {
                exit::PASSED
            } else {
                exit::FAILED
            })
        }
        ExpCmd::List => {
            for e in experiment::list(&store)? {
                println!(
                    "{}  best={}  protocol={}  {}",
                    e.name,
                    e.best,
                    e.protocol_hash,
                    render::clean(&e.hypothesis)
                );
            }
            Ok(exit::PASSED)
        }
    }
}

fn main() -> ExitCode {
    // `prova ... | head` closes stdout early; exit quietly like other tools.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<String>()
            .map(String::as_str)
            .unwrap_or("");
        if msg.contains("Broken pipe") {
            std::process::exit(141);
        }
        default_hook(info);
    }));
    let cli = Cli::parse();
    match dispatch(cli.cmd) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("prova: error: {e:#}");
            ExitCode::from(exit::INFRA)
        }
    }
}

fn open_here() -> Result<(Store, PathBuf, PathBuf)> {
    let cwd = std::env::current_dir()?;
    let root = config::find_root(&cwd);
    let store =
        Store::open(&root).with_context(|| format!("opening store for {}", root.display()))?;
    if store.is_fallback() {
        eprintln!(
            "prova: warning: prova home not writable (set PROVA_HOME); history kept in {}",
            store.dir().display()
        );
    }
    Ok((store, root, cwd))
}

fn until_name(u: UntilArg) -> &'static str {
    match u {
        UntilArg::Done => "the result",
        UntilArg::Stall => "a stall",
        UntilArg::FirstFailure => "the first failure",
    }
}

/// How long one wait may last (ADR 0019): the caller's --max-wait, else 9
/// minutes for the result (one tool call's usual limit is 10), else 30 s.
fn wait_budget(until: UntilArg, max_wait: Option<&str>) -> Result<std::time::Duration> {
    match max_wait {
        Some(m) => parse_duration(m).ok_or_else(|| anyhow!("invalid --max-wait {m:?}")),
        None if matches!(until, UntilArg::Done) => Ok(std::time::Duration::from_secs(9 * 60)),
        None => Ok(std::time::Duration::from_secs(30)),
    }
}

/// Arguments as a shell would need them typed (`-run '^$'`), for hints the
/// agent copies.
fn sh_quote(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            let plain = !a.is_empty()
                && a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_./=:,@%+-".contains(c));
            if plain {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `prova output`: the live spool while the run is going, then what was
/// kept (ADR 0016).
fn print_output(
    store: &Store,
    id: &str,
    tail: Option<usize>,
    test: Option<&str>,
    json: bool,
) -> Result<u8> {
    use prova_core::output::{self, Kept};
    if let Some(name) = test {
        let found: Vec<(String, String)> = output::test_outputs(store, id)?
            .into_iter()
            .filter(|(d, _)| d.contains(name))
            .collect();
        if found.is_empty() {
            bail!(
                "no kept output for a failing test matching {name:?} in run {id}: a test cut off before it finished, or a framework that printed nothing per test, has none; `prova output {id}` shows the run's kept output"
            );
        }
        if json {
            let v: Vec<_> = found
                .iter()
                .map(|(d, t)| serde_json::json!({ "test": d, "output": t }))
                .collect();
            println!("{}", serde_json::to_string(&v)?);
        } else {
            for (d, t) in found {
                println!("== {d}\n{t}");
            }
        }
        return Ok(exit::PASSED);
    }
    if let Some(n) = tail {
        for l in engine::output_tail(store, id, n) {
            println!("{l}");
        }
        return Ok(exit::PASSED);
    }
    let spool = store.run_dir(id).join("output.log");
    if !store.run(id)?.status.is_terminal() && spool.exists() {
        let mut f = std::fs::File::open(&spool).with_context(|| format!("{}", spool.display()))?;
        std::io::copy(&mut f, &mut std::io::stdout())?;
        return Ok(exit::PASSED);
    }
    if let Some(full) = output::run_full(store, id)? {
        print!("{full}");
        return Ok(exit::PASSED);
    }
    let tests = output::test_outputs(store, id)?;
    match output::run_excerpt(store, id)? {
        Kept::Excerpt(e) if json => {
            let v = serde_json::json!({
                "run": id,
                "excerpt": e,
                "elided_lines": e.total_lines - e.kept_lines(),
                "failing_tests_with_output": tests.iter().map(|(d, _)| d).collect::<Vec<_>>(),
            });
            println!("{}", serde_json::to_string(&v)?);
        }
        Kept::Excerpt(e) => {
            print!("{}", e.render());
            if !tests.is_empty() {
                println!(
                    "-- kept output of {} failing test(s); `prova output {id} --test <name>`:",
                    tests.len()
                );
                for (d, _) in &tests {
                    println!("   {d}");
                }
            }
        }
        Kept::Pruned => println!(
            "output of run {id} was dropped to stay within the output budget; results and failures are kept"
        ),
        Kept::Missing => println!("no output kept for run {id}"),
    }
    Ok(exit::PASSED)
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
            inject,
            keep_output,
            until,
            max_wait,
            json,
            command,
        } => {
            let (store, root, cwd) = open_here()?;
            let cfg = config::load(&root)?;
            // `prova run -- make test --detach` passes --detach to make.
            const OWN_FLAGS: &[&str] = &[
                "--detach",
                "--json",
                "--select",
                "--inject",
                "--stall-after",
                "--kill-after",
                "--keep-output",
            ];
            if let Some(f) = command
                .iter()
                .find(|a| OWN_FLAGS.contains(&a.split('=').next().unwrap_or(a)))
            {
                eprintln!(
                    "prova: note: {f} comes after `--`, so it goes to the command, not to prova; put prova's flags before `--`"
                );
            }
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
                    inject: Vec::new(),
                    keep_output: false,
                    expect_tests: None,
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
                        "nothing to run: give a command (`prova run -- make test`) or a suite name{}",
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
            for name in inject {
                if !spec.inject.contains(&name) {
                    spec.inject.push(name);
                }
            }
            spec.keep_output |= keep_output;
            prova_adapters::inject::validate(&spec.inject).map_err(|e| anyhow!(e))?;
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
            if let Some(until) = until {
                // One command for "run this and tell me" (ADR 0019).
                let max = wait_budget(until, max_wait.as_deref())?;
                if !json {
                    eprintln!(
                        "run {id} started; waiting up to {} for {}",
                        prova_core::ids::format_duration(max),
                        until_name(until)
                    );
                }
                let s = wait_until(&store, &id, until, 0, max)?;
                render::summary_until(&s, json, Some(until.into()));
                return Ok(exit_code(&s));
            }
            if detach {
                if json {
                    println!("{}", serde_json::json!({ "run": id }));
                } else {
                    // stdout carries only the id, so `r=$(prova run --detach)`
                    // works; the human hint goes to stderr.
                    println!("{id}");
                    eprintln!("run {id} started");
                    eprintln!("next: prova wait {id}");
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
            let max = wait_budget(until, max_wait.as_deref())?;
            let s = wait_until(&store, &id, until, after, max)?;
            render::summary_until(&s, json, Some(until.into()));
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
            println!("next: prova wait {id} --until done");
            Ok(exit::PASSED)
        }
        Cmd::Status {
            all: true, json, ..
        } => {
            let mut out = Vec::new();
            for k in prova_core::home::known_repos() {
                let store = Store::open_dir(Path::new(&k.workspace), &k.dir)?;
                let mut runs = store.recent_runs(5)?;
                if engine::reap_orphans(&store, &runs)? {
                    runs = store.recent_runs(5)?;
                }
                if json {
                    out.push(serde_json::json!({ "repo": k, "runs": runs }));
                } else if !runs.is_empty() {
                    println!("== {}  ({})", k.name, k.workspace);
                    render::runs(&runs, false);
                }
            }
            if json {
                println!("{}", serde_json::to_string(&out)?);
            }
            Ok(exit::PASSED)
        }
        Cmd::Status { run, json, .. } => {
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
                    let mut runs = store.recent_runs(20)?;
                    if engine::reap_orphans(&store, &runs)? {
                        runs = store.recent_runs(20)?;
                    }
                    render::runs(&runs, json);
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
                    ended |= matches!(e.kind, prova_core::model::EventKind::RunFinished { .. });
                }
                if !follow || ended {
                    return Ok(exit::PASSED);
                }
                // Reuse wait's orphan detection and bounded blocking.
                let _ = engine::wait(&store, &id, Until::Done, cursor, Duration::from_millis(500))?;
            }
        }
        Cmd::Output {
            run,
            tail,
            test,
            json,
        } => {
            let (store, _, _) = open_here()?;
            let id = store.resolve_run(&run)?;
            print_output(&store, &id, tail, test.as_deref(), json)
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
            all: true,
            window,
            json,
        } => {
            let mut out = Vec::new();
            for k in prova_core::home::known_repos() {
                let store = Store::open_dir(Path::new(&k.workspace), &k.dir)?;
                let list = analysis::flaky(&store, suite.as_deref(), window)?;
                if json {
                    out.push(serde_json::json!({ "repo": k, "flaky": list }));
                } else {
                    for t in &list {
                        println!(
                            "{:20} {:5.1}% fail  {}",
                            k.name,
                            t.failure_rate * 100.0,
                            render::clean(&t.display)
                        );
                    }
                }
            }
            if json {
                println!("{}", serde_json::to_string(&out)?);
            }
            Ok(exit::PASSED)
        }
        Cmd::Flaky {
            suite,
            window,
            json,
            ..
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
        Cmd::Sizes { suite, json } => {
            let (store, _, _) = open_here()?;
            let d = analysis::sizes(&store, suite.as_deref())?;
            if json {
                println!("{}", serde_json::to_string(&d)?);
            } else {
                println!(
                    "{} tests by observed p95 duration: {} small (<=1s), {} medium (<=30s), {} large",
                    d.total, d.small, d.medium, d.large
                );
                if let Some(w) = &d.warning {
                    println!("warning: {w}");
                }
                if d.total == 0 {
                    println!(
                        "no per-test durations recorded yet: the framework output carries none (e.g. plain `cargo test`); a JUnit report (nextest, pytest --junitxml) or go test -json provides them"
                    );
                }
                for (t, ms) in d.largest.iter().take(10) {
                    println!("  {ms:>8}ms  {}", render::clean(t));
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
        Cmd::Exp(c) => exp(c),
        Cmd::Mcp => {
            mcp::serve()?;
            Ok(exit::PASSED)
        }
        Cmd::Init { claude } => init(claude),
        Cmd::Adopt {
            write,
            verify,
            filter,
            json,
        } => adopt(write, verify, filter, json),
        Cmd::ExpWorker {
            root,
            dir,
            comparison,
        } => {
            let store = Store::open_dir(&root, &dir)?;
            experiment::work(&store, &comparison, &prova_adapters::bench::parse)?;
            Ok(exit::PASSED)
        }
        Cmd::Supervise { root, dir, run } => {
            let store = Store::open_dir(&root, &dir)?;
            let row = store.run(&run)?;
            let spec: RunSpec = serde_json::from_value(row.spec)?;
            let mut observer = prova_adapters::observer_for(&spec, &root);
            engine::supervise(&store, &run, observer.as_mut())?;
            Ok(exit::PASSED)
        }
    }
}

/// Attached run: wait loops until the end, reporting significant events.
/// Waits for `until` within `budget` (ADR 0019). Short waits for the first
/// failure return as they are; a wait for the result or a stall shows
/// progress on stderr for the person watching: every new failure, slow
/// test and stall as it happens, and one line every 30 s.
fn wait_until(
    store: &Store,
    id: &str,
    until: UntilArg,
    after: u64,
    budget: Duration,
) -> Result<Summary> {
    if matches!(until, UntilArg::FirstFailure) {
        return Ok(engine::wait(store, id, Until::FirstFailure, after, budget)?);
    }
    let start = std::time::Instant::now();
    let mut beat = std::time::Instant::now();
    let mut cursor = after;
    let mut reported: std::collections::BTreeSet<String> = Default::default();
    loop {
        let left = budget.saturating_sub(start.elapsed());
        if left.is_zero() {
            return Ok(engine::summarize(store, id, WaitReason::Running, cursor)?);
        }
        let s = engine::wait(
            store,
            id,
            Until::FirstFailure,
            cursor,
            left.min(Duration::from_secs(1)),
        )?;
        cursor = s.last_seq;
        match s.reason {
            WaitReason::Done => return Ok(s),
            WaitReason::Stall if matches!(until, UntilArg::Stall) => return Ok(s),
            WaitReason::Failure | WaitReason::Stall | WaitReason::Slow => {
                render::progress(&s, &mut reported)
            }
            WaitReason::Running => {}
        }
        if beat.elapsed() >= Duration::from_secs(30) {
            beat = std::time::Instant::now();
            render::heartbeat(&s);
        }
    }
}

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
            WaitReason::Failure | WaitReason::Stall | WaitReason::Slow if !json => {
                render::progress(&s, &mut reported)
            }
            _ => {}
        }
    }
}

fn exit_code(s: &Summary) -> u8 {
    match s.reason {
        WaitReason::Running => exit::RUNNING,
        WaitReason::Stall | WaitReason::Slow => exit::STALLED_OR_KILLED,
        WaitReason::Failure => exit::FAILED,
        WaitReason::Done => match (s.outcome, s.status) {
            (Some(Outcome::Passed), _) => exit::PASSED,
            (Some(Outcome::Failed), _) => exit::FAILED,
            // The command could not start: not a test failure (ADR 0017).
            (Some(Outcome::Errored), Status::Finished)
                if matches!(s.exit_code, Some(126 | 127)) =>
            {
                exit::INFRA
            }
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
mod quoting_tests {
    #[test]
    fn hints_quote_what_a_shell_would_eat() {
        let a: Vec<String> = ["go", "test", "-run", "^$", "-bench", "X", "it's"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(super::sh_quote(&a), "go test -run '^$' -bench X 'it'\\''s'");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn usage_exit_code_matches_clap() {
        assert_eq!(super::exit::USAGE, 2);
    }
}
