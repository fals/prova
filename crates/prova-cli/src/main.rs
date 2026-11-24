//! `prova` command line (ADR 0009). Primary conduit for agents and humans.

mod mcp;
mod render;
mod skills;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};
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

const ROOT_EXAMPLES: &str = "\
Examples:
  prova run -- make test                   any test command, no config
  prova run unit --until first-failure     a suite from prova.toml; back at the first failure
  prova output                             what the last run printed
  prova history calc::divides              new failure, pre-existing, or flaky?
  prova exp new speed --hypothesis \"...\" -- go test -bench .

Shell completion (commands, options, suite names, run ids):
  bash  echo 'source <(COMPLETE=bash prova)' >> ~/.bashrc
  zsh   echo 'source <(COMPLETE=zsh prova)' >> ~/.zshrc
  fish  echo 'COMPLETE=fish prova | source' >> ~/.config/fish/config.fish
  (or `prova completions <shell>` prints the script)";

#[derive(Parser)]
#[command(
    name = "prova",
    version,
    about = "prova: tests with memory, experiments with proof",
    after_help = ROOT_EXAMPLES
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
    #[command(after_help = "\
Examples:
  prova run -- make test                     any command; history starts now
  prova run unit --until first-failure       back at the first failure, a slow test or a stall (30s)
  prova run unit --until done                the result (waits up to 9m, then prints `next:`)
  prova run unit --select calc::divides      rerun one test (the suite's `select` template)
  prova run unit --repeat 10                 is it flaky? each test's pass/fail tally
  prova run unit --detach                    start, print the run id, return; then `prova wait`")]
    Run {
        /// Suite name from prova.toml (omit it and give a command after `--`).
        #[arg(add = ArgValueCompleter::new(complete_suites))]
        suite: Option<String>,
        /// Run only tests matching this filter (uses the suite's `select`).
        #[arg(long, value_name = "TEST")]
        select: Option<String>,
        /// Return the run id immediately; follow with `prova wait`.
        #[arg(long, conflicts_with = "until")]
        detach: bool,
        /// Start the run and wait for this in one command:
        /// `done` (the result), `first-failure`, or `stall`.
        #[arg(long, value_enum)]
        until: Option<UntilArg>,
        /// With --until: wait at most this long (default 9m for `done`,
        /// 30s otherwise); then exit 5 with the command to continue.
        #[arg(long, value_name = "DURATION")]
        max_wait: Option<String>,
        /// Run it N times in a row and report each test's tally (is it
        /// flaky?), within one --max-wait budget (default 9m).
        #[arg(long, value_name = "N", conflicts_with = "detach")]
        repeat: Option<u32>,
        /// Report a stall after this long without activity (e.g. 30s).
        /// Default: the suite's `stall_after`, else by size (small 30s,
        /// medium 60s, large 5m), else 60s.
        #[arg(long, value_name = "DURATION")]
        stall_after: Option<String>,
        /// Kill the run after this long in total (e.g. 30m).
        #[arg(long, value_name = "DURATION")]
        kill_after: Option<String>,
        /// Opt-in environment injection recipe (repeatable), e.g. pytest-junit.
        #[arg(long = "inject", value_name = "RECIPE")]
        inject: Vec<String>,
        /// Keep the whole output (compressed), not only the excerpt: for
        /// debugging prova's recognizers.
        #[arg(long)]
        keep_output: bool,
        /// One JSON object instead of text (same fields).
        #[arg(long)]
        json: bool,
        /// Command to run, after `--`.
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Wait for the next significant event of a run, at most --max-wait.
    #[command(after_help = "\
Examples:
  prova wait                         the latest run: first failure, slow test, stall or end
  prova wait 3fa9 --until done       its result (up to 9m)
  prova wait 3fa9 --after 14         only events after #14 (the `next:` line gives it)")]
    Wait {
        /// Run id, a unique prefix of it, or `last`.
        #[arg(default_value = "last", add = ArgValueCompleter::new(complete_runs))]
        run: String,
        /// What to wait for.
        #[arg(long, value_enum, default_value = "first-failure")]
        until: UntilArg,
        /// Default: 9m for `--until done` (under the usual 10-minute limit of
        /// one tool call), 30s otherwise.
        #[arg(long, value_name = "DURATION")]
        max_wait: Option<String>,
        /// Only consider events after this sequence number.
        #[arg(long, value_name = "SEQ", default_value_t = 0)]
        after: u64,
        /// One JSON object instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Cancel a run (process group: SIGTERM, grace, SIGKILL).
    #[command(after_help = "\
Examples:
  prova cancel 3fa9c1d2
  prova cancel last")]
    Cancel {
        /// Run id, a unique prefix of it, or `last`.
        #[arg(add = ArgValueCompleter::new(complete_runs))]
        run: String,
    },
    /// Take a run out of every test's history (a run whose results were
    /// not real), with the reason; flaky labels it caused go away.
    #[command(after_help = "\
Example:
  prova forget 3fa9 --reason \"ran against a stale build\"")]
    Forget {
        /// Run id, a unique prefix of it, or `last`.
        #[arg(add = ArgValueCompleter::new(complete_runs))]
        run: String,
        /// Why its results were not real (kept with the run).
        #[arg(long)]
        reason: String,
    },
    /// Show one run, or recent runs.
    #[command(after_help = "\
Examples:
  prova status              recent runs of this repository
  prova status last         the latest run: result, warnings, failures
  prova status --all        recent runs of every repository")]
    Status {
        /// Run id, a unique prefix of it, or `last`; omit for recent runs.
        #[arg(add = ArgValueCompleter::new(complete_runs))]
        run: Option<String>,
        /// Recent runs of every repository known to this prova home.
        #[arg(long, conflicts_with = "run")]
        all: bool,
        /// JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Print a run's events as JSON Lines.
    #[command(after_help = "\
Examples:
  prova events                    the latest run's events
  prova events 3fa9 --follow      keep printing until the run ends
  prova events 3fa9 --after 14    only events after #14")]
    Events {
        /// Run id, a unique prefix of it, or `last`.
        #[arg(default_value = "last", add = ArgValueCompleter::new(complete_runs))]
        run: String,
        /// Only events after this sequence number.
        #[arg(long, value_name = "SEQ", default_value_t = 0)]
        after: u64,
        /// Keep printing new events until the run ends.
        #[arg(long)]
        follow: bool,
    },
    /// Print a run's output: live while it runs; once it ended, the kept
    /// excerpt (head, error blocks, tail) and failing tests' own output.
    #[command(after_help = "\
Examples:
  prova output                          the latest run's kept output
  prova output 3fa9                     a run by id (or a unique prefix)
  prova output last --tail 50           only its last 50 lines
  prova output last --test divides      one failing test's own output
  prova output 3fa9 --json              the same, as JSON")]
    Output {
        /// Run id, a unique prefix of it, or `last`.
        #[arg(default_value = "last", add = ArgValueCompleter::new(complete_runs))]
        run: String,
        /// Last lines only.
        #[arg(long, value_name = "N")]
        tail: Option<usize>,
        /// One failing test's own output (substring of its name).
        #[arg(long, value_name = "TEST")]
        test: Option<String>,
        /// JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// History and flake statistics of tests matching a name.
    #[command(after_help = "\
Examples:
  prova history calc::divides           one test: outcomes, code versions, size, flakiness
  prova history divides --suite unit    only runs of one suite
  prova history \"\"                      every test prova knows")]
    History {
        /// Substring of the test's full name.
        test: String,
        /// Only runs of this suite.
        #[arg(long, add = ArgValueCompleter::new(complete_suites))]
        suite: Option<String>,
        /// How many of the latest runs to look at.
        #[arg(long, value_name = "RUNS", default_value_t = 50)]
        window: usize,
        /// JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Tests with flaky evidence: passed on retry, or both passed and failed on the same code.
    #[command(after_help = "\
Examples:
  prova flaky                  this repository
  prova flaky --suite unit     one suite
  prova flaky --all            every repository")]
    Flaky {
        /// Only runs of this suite.
        #[arg(long, add = ArgValueCompleter::new(complete_suites))]
        suite: Option<String>,
        /// Across every repository known to this prova home.
        #[arg(long)]
        all: bool,
        /// How many of the latest runs to look at.
        #[arg(long, value_name = "RUNS", default_value_t = 100)]
        window: usize,
        /// JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Test sizes from what each test used (processes, network), with the reason (ADR 0025).
    #[command(after_help = "\
Examples:
  prova sizes                  counts, and each medium or large test with what it used
  prova sizes --suite unit     one suite")]
    Sizes {
        /// Only runs of this suite.
        #[arg(long, add = ArgValueCompleter::new(complete_suites))]
        suite: Option<String>,
        /// JSON instead of text (every test).
        #[arg(long)]
        json: bool,
    },
    /// Compare per-test results of two runs.
    #[command(after_help = "\
Example:
  prova diff 3fa9 last         fixed, broken and new tests between two runs")]
    Diff {
        /// The earlier run: id, unique prefix, or `last`.
        #[arg(add = ArgValueCompleter::new(complete_runs))]
        base: String,
        /// The later run: id, unique prefix, or `last`.
        #[arg(add = ArgValueCompleter::new(complete_runs))]
        head: String,
        /// JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Install prova skills for agents and add a short section to AGENTS.md.
    #[command(after_help = "\
Example:
  prova init       in the repository root; safe to run again")]
    Init {
        /// Kept for compatibility: skills now always go to .claude/skills too.
        #[arg(long, hide = true)]
        claude: bool,
    },
    /// Detect the test setup and propose prova.toml; verify single-test selection.
    #[command(after_help = "\
Examples:
  prova adopt                                    print the proposal
  prova adopt --write                            write prova.toml (only if none exists)
  prova adopt --verify unit --filter divides     check that `select` runs only that test")]
    Adopt {
        /// Write the proposal to prova.toml (only if none exists).
        #[arg(long)]
        write: bool,
        /// Verify the `select` template of this suite.
        #[arg(long, value_name = "SUITE", requires = "filter", add = ArgValueCompleter::new(complete_suites))]
        verify: Option<String>,
        /// A real test name to select when verifying.
        #[arg(long, value_name = "TEST")]
        filter: Option<String>,
        /// JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Serve the MCP facade on stdio (tools mirror the CLI).
    #[command(after_help = "\
Example:
  claude mcp add prova -- prova mcp")]
    Mcp,
    /// Print the shell completion script (commands, options, suite names,
    /// run ids, experiments).
    #[command(after_help = "\
Examples:
  echo 'source <(prova completions bash)' >> ~/.bashrc
  echo 'source <(prova completions zsh)' >> ~/.zshrc
  echo 'prova completions fish | source' >> ~/.config/fish/config.fish")]
    Completions {
        /// The shell.
        #[arg(value_parser = ["bash", "zsh", "fish", "elvish", "powershell"])]
        shell: String,
    },
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
    /// Primary metric (see `prova exp metrics`).
    #[arg(long)]
    metric: Option<String>,
    /// Whether lower or higher is better.
    #[arg(long, value_enum)]
    better: Option<BetterArg>,
    /// Unit of the metric, for reports.
    #[arg(long)]
    unit: Option<String>,
    /// Smallest change that matters, e.g. 2% or 0.02.
    #[arg(long)]
    min_effect: Option<String>,
    /// Significance level.
    #[arg(long)]
    alpha: Option<f64>,
    /// Trials per arm before the first verdict.
    #[arg(long)]
    trials: Option<u32>,
    /// Fewest trials per arm for any verdict.
    #[arg(long)]
    min_trials: Option<u32>,
    /// Inconclusive results extend up to this many trials per arm.
    #[arg(long)]
    max_trials: Option<u32>,
    /// Unmeasured runs per arm first.
    #[arg(long)]
    warmup: Option<u32>,
    /// Replaces the guardrails (NAME or NAME:higher), repeatable.
    #[arg(long = "guardrail")]
    guardrails: Vec<String>,
    /// Kill a single trial after this long.
    #[arg(long)]
    kill_after: Option<String>,
    /// Shell command run once in each variant's directory before trials.
    #[arg(long)]
    setup: Option<String>,
    /// Adds a specification file (repeatable).
    #[arg(long = "spec")]
    specs: Vec<PathBuf>,
    /// Removes a specification file from the protocol, before the first
    /// trial (repeatable), e.g. `--no-spec spec/old.lean`.
    #[arg(long = "no-spec")]
    no_specs: Vec<PathBuf>,
    /// Command that must succeed for a variant to satisfy the specification.
    #[arg(long)]
    oracle: Option<String>,
    /// Replaces the extra paths a candidate may change (glob, repeatable).
    #[arg(long = "change")]
    scope: Vec<String>,
}

#[derive(clap::Args)]
struct ProtocolArgs {
    /// Primary metric: a JSON line {"metric": NAME, "value": N} the workload
    /// prints, or one read from benchmark output (go test -bench, criterion,
    /// pytest-benchmark, Vitest, JMH, BenchmarkDotNet). `prova exp metrics -- <cmd>`
    /// lists them; `wall_ms` is always recorded.
    #[arg(long, default_value = "wall_ms")]
    metric: String,
    /// Whether lower or higher is better.
    #[arg(long, value_enum, default_value = "lower")]
    better: BetterArg,
    /// Unit of the metric, for reports.
    #[arg(long)]
    unit: Option<String>,
    /// Smallest change that matters, e.g. 2% or 0.02.
    #[arg(long, default_value = "2%")]
    min_effect: String,
    /// Significance level.
    #[arg(long, default_value_t = 0.05)]
    alpha: f64,
    /// Trials per arm before the first verdict.
    #[arg(long, default_value_t = 10)]
    trials: u32,
    /// Fewest trials per arm for any verdict.
    #[arg(long, default_value_t = 5)]
    min_trials: u32,
    /// Inconclusive results extend up to this many trials per arm.
    #[arg(long, default_value_t = 20)]
    max_trials: u32,
    /// Unmeasured runs per arm first.
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
    /// Also allow a candidate to change these paths (glob: `*` within a
    /// directory, `**` across; repeatable), e.g. `--change go.mod`. By
    /// default a candidate may change anything but tests, benchmarks, build
    /// and runner files.
    #[arg(long = "change")]
    scope: Vec<String>,
    /// With --change: the candidate may change only those paths.
    #[arg(long = "change-only", requires = "scope")]
    scope_only: bool,
    /// Let trials and the oracle use the network (by default a sandboxed
    /// arm has only loopback).
    #[arg(long)]
    allow_network: bool,
}

#[derive(Subcommand)]
enum ExpCmd {
    /// Set the best variant by hand, e.g. back to `baseline` after an
    /// ACCEPT that turned out wrong; recorded in the decision log.
    #[command(after_help = "\
Example:
  prova exp best speed baseline --reason \"c2's win did not replicate\"")]
    Best {
        /// Experiment name.
        #[arg(add = ArgValueCompleter::new(complete_experiments))]
        name: String,
        /// Variant to make the best: `baseline` or a candidate (c1, ...).
        variant: String,
        /// Why (kept in the decision log).
        #[arg(long)]
        reason: String,
    },
    /// Create an experiment; the baseline is a git ref (default HEAD).
    #[command(after_help = "\
Examples:
  prova exp new speed --hypothesis \"sieve beats trial division\" \\
    --metric BenchmarkPrimes.ns_per_op --min-effect 5% -- go test -run '^$' -bench . -count 3
  prova exp new p95 --hypothesis \"...\" --metric latency_ms --guardrail rss_mb -- ./bench.sh")]
    New {
        /// A short name for the experiment, e.g. `speed`.
        name: String,
        /// What you expect and why, fixed before any trial.
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
    #[command(after_help = "\
Example:
  prova exp protocol speed --trials 20 --min-effect 3%")]
    Protocol {
        /// Experiment name.
        #[arg(add = ArgValueCompleter::new(complete_experiments))]
        name: String,
        #[command(flatten)]
        change: ProtocolChange,
    },
    /// Record a candidate: the working tree now (default) or a git ref.
    #[command(after_help = "\
Examples:
  prova exp candidate speed                      the working tree as it is now
  prova exp candidate speed --ref feature/sieve
  prova exp candidate speed --param THREADS=8")]
    Candidate {
        /// Experiment name.
        #[arg(add = ArgValueCompleter::new(complete_experiments))]
        name: String,
        /// Candidate name (default c1, c2, ...).
        #[arg(long)]
        label: Option<String>,
        /// Record this git ref instead of the working tree.
        #[arg(long = "ref")]
        git_ref: Option<String>,
        /// Parameter passed to the workload as PROVA_PARAM_<KEY> (KEY=VALUE).
        #[arg(long = "param", value_name = "KEY=VALUE")]
        params: Vec<String>,
    },
    /// Run trials of a candidate against the current best and decide.
    #[command(after_help = "\
Examples:
  prova exp run speed                        the latest candidate against the best
  prova exp run speed --candidate c2 --detach")]
    Run {
        /// Experiment name.
        #[arg(add = ArgValueCompleter::new(complete_experiments))]
        name: String,
        /// Candidate (default: the latest recorded).
        #[arg(long)]
        candidate: Option<String>,
        /// Compare against this variant instead of the current best.
        #[arg(long)]
        against: Option<String>,
        /// Start the trials, print and return; then `prova exp wait`.
        #[arg(long)]
        detach: bool,
        /// Longest foreground wait (default 9m), then `exp wait` continues.
        #[arg(long, value_name = "DURATION")]
        max_wait: Option<String>,
        /// JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Re-measure the current best against the original baseline.
    #[command(after_help = "\
Example:
  prova exp revalidate speed")]
    Revalidate {
        /// Experiment name.
        #[arg(add = ArgValueCompleter::new(complete_experiments))]
        name: String,
        /// Start the trials, print and return; then `prova exp wait`.
        #[arg(long)]
        detach: bool,
        /// Longest foreground wait (default 9m), then `exp wait` continues.
        #[arg(long, value_name = "DURATION")]
        max_wait: Option<String>,
        /// JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Wait for the running comparison, at most --max-wait.
    #[command(after_help = "\
Example:
  prova exp wait speed --max-wait 5m")]
    Wait {
        /// Experiment name.
        #[arg(add = ArgValueCompleter::new(complete_experiments))]
        name: String,
        /// Wait at most this long.
        #[arg(long, value_name = "DURATION", default_value = "30s")]
        max_wait: String,
        /// JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Show the experiment, latest comparison and decision log.
    #[command(after_help = "\
Example:
  prova exp status speed")]
    Status {
        /// Experiment name.
        #[arg(add = ArgValueCompleter::new(complete_experiments))]
        name: String,
        /// JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Cancel the running comparison.
    #[command(after_help = "\
Example:
  prova exp cancel speed")]
    Cancel {
        /// Experiment name.
        #[arg(add = ArgValueCompleter::new(complete_experiments))]
        name: String,
    },
    /// List experiments.
    #[command(after_help = "\
Example:
  prova exp list        name, best variant, protocol hash, hypothesis")]
    List,
    /// Kept logs of what failed in the latest comparison (a trial, the
    /// setup, the oracle).
    #[command(after_help = "\
Examples:
  prova exp log speed                    every kept log
  prova exp log speed oracle-candidate   one of them
  prova exp log speed 3 --tail 20        trial 3, last 20 lines")]
    Log {
        /// Experiment name.
        #[arg(add = ArgValueCompleter::new(complete_experiments))]
        name: String,
        /// One log: a trial number, setup-baseline, oracle-candidate, ...
        entry: Option<String>,
        /// Only the last N kept lines of each log.
        #[arg(long)]
        tail: Option<usize>,
    },
    /// Run a workload once and show the metrics prova reads from it
    /// (benchmark tools and JSON metric lines), to choose --metric.
    #[command(after_help = "\
Example:
  prova exp metrics -- go test -run '^$' -bench . -count 1")]
    Metrics {
        /// JSON instead of text.
        #[arg(long)]
        json: bool,
        /// Workload command, after `--`.
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
}

// ---------------------------------------------------------------- completion
//
// `COMPLETE=<shell> prova` prints the shell's completion script; the shell
// then calls prova back for candidates. Completers only read: a TAB press
// never creates a store.

fn complete_suites(current: &std::ffi::OsStr) -> Vec<CompletionCandidate> {
    let Ok(cwd) = std::env::current_dir() else {
        return Vec::new();
    };
    let Ok(cfg) = config::load(&config::find_root(&cwd)) else {
        return Vec::new();
    };
    let prefix = current.to_string_lossy();
    cfg.suite
        .iter()
        .filter(|(n, _)| n.starts_with(prefix.as_ref()))
        .map(|(n, s)| CompletionCandidate::new(n).help(Some(s.run.clone().into())))
        .collect()
}

/// The store of this repository, if one exists (opened read-mostly: no
/// store is created for a completion).
fn existing_store() -> Option<Store> {
    let cwd = std::env::current_dir().ok()?;
    let root = config::find_root(&cwd);
    let dir = prova_core::home::existing(&root)?;
    Store::open_dir(&root, &dir).ok()
}

fn complete_runs(current: &std::ffi::OsStr) -> Vec<CompletionCandidate> {
    let prefix = current.to_string_lossy();
    let mut out = vec![CompletionCandidate::new("last").help(Some("the latest run".into()))];
    if let Some(store) = existing_store()
        && let Ok(runs) = store.recent_runs(20)
    {
        out.extend(runs.into_iter().map(|r| {
            let outcome = r
                .outcome
                .map_or_else(|| format!("{:?}", r.status), |o| format!("{o:?}"));
            CompletionCandidate::new(r.id).help(Some(
                format!("{} {}", r.suite, outcome.to_lowercase()).into(),
            ))
        }));
    }
    out.retain(|c| c.get_value().to_string_lossy().starts_with(prefix.as_ref()));
    out
}

fn complete_experiments(current: &std::ffi::OsStr) -> Vec<CompletionCandidate> {
    let prefix = current.to_string_lossy();
    let Some(store) = existing_store() else {
        return Vec::new();
    };
    experiment::list(&store)
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.name.starts_with(prefix.as_ref()))
        .map(|e| CompletionCandidate::new(e.name).help(Some(e.hypothesis.into())))
        .collect()
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

/// Paths the oracle command names (existing files or directories of the
/// repository) that no `--spec` covers: the oracle's own files are part of
/// the specification, and a candidate may edit them unless they are frozen
/// (usability round 13).
fn unfrozen_oracle_paths(p: &experiment::Protocol, root: &Path) -> Vec<String> {
    let Some(oracle) = &p.oracle else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for tok in oracle.split_whitespace() {
        let t = tok.trim_matches(|c| matches!(c, '"' | '\'' | ';' | '(' | ')' | '&' | '|'));
        let t = t.strip_prefix("./").unwrap_or(t);
        if t.is_empty() || t.starts_with('-') || t.starts_with('/') || t.contains("..") || t == "."
        {
            continue;
        }
        let t = t.trim_end_matches('/');
        let path = root.join(t);
        let covered = p
            .specs
            .iter()
            .any(|s| s.path == t || (path.is_dir() && s.path.starts_with(&format!("{t}/"))));
        if path.exists() && !covered && !out.iter().any(|o| o == t) {
            out.push(t.to_owned());
        }
    }
    out
}

fn warn_unfrozen_oracle_paths(p: &experiment::Protocol, root: &Path) {
    let paths = unfrozen_oracle_paths(p, root);
    if paths.is_empty() {
        return;
    }
    let flags: Vec<String> = paths.iter().map(|x| format!("--spec {x}")).collect();
    eprintln!(
        "warning: the oracle names {} that no --spec freezes, so a candidate can change what judges it; add {} (files only: name the files inside a directory; remove one with `prova exp protocol <name> --no-spec <file>`)",
        paths.join(", "),
        flags.join(" ")
    );
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
        scope: a.scope.clone(),
        scope_only: a.scope_only,
        allow_network: a.allow_network,
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

/// `exp status` exit: the experiment's state. A REJECT after an accepted best
/// is not a failure of the experiment; a running comparison stays 5.
fn exp_status_exit(r: &experiment::Report) -> u8 {
    let code = exp_exit(r);
    let rejected = r
        .comparison
        .as_ref()
        .is_some_and(|c| c.status == "decided" && c.verdict.as_deref() == Some("reject"));
    if rejected && r.best != "baseline" {
        exit::PASSED
    } else {
        code
    }
}

fn exp_start(
    store: &Store,
    name: &str,
    candidate: &str,
    against: Option<&str>,
    detach: bool,
    max_wait: Option<&str>,
    json: bool,
) -> Result<u8> {
    let budget = match max_wait {
        Some(m) => parse_duration(m).ok_or_else(|| anyhow!("invalid --max-wait {m:?}"))?,
        None => Duration::from_secs(9 * 60),
    };
    let id = experiment::start(store, name, candidate, against)?;
    let best = experiment::get(store, name)?.best;
    let live = experiment::live_tree_variants(store, name, &[candidate, against.unwrap_or(&best)]);
    if !live.is_empty() {
        eprintln!(
            "warning: {} has no snapshot (recorded before snapshots, or outside git) and runs in the live working tree; record it again with `prova exp candidate {name}` to pin it",
            live.join(", ")
        );
    }
    // Holm: each decided comparison raises the bar for the next one, and few
    // trials have a floor; say when ACCEPT has become impossible (round 6).
    let e = experiment::get(store, name)?;
    let decided = experiment::decided_comparisons(store, name)? as i64;
    let floor = prova_core::stats::min_two_sided_p(
        e.protocol.max_trials as usize,
        e.protocol.max_trials as usize,
    );
    if floor * (decided as f64 + 1.0) >= e.protocol.alpha {
        eprintln!(
            "warning: this is comparison {} of experiment {name}; with at most {} + {} trials the adjusted p cannot go below {:.4} (alpha {}), so this comparison cannot be accepted; start a new experiment with more --max-trials",
            decided + 1,
            e.protocol.max_trials,
            e.protocol.max_trials,
            floor * (decided as f64 + 1.0),
            e.protocol.alpha
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
    // Bounded like `prova run --until done` (round 11: a foreground
    // comparison blocked with no limit): 9 minutes, then the command to
    // continue; the comparison goes on in the background.
    let deadline = std::time::Instant::now() + budget;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let r = experiment::wait(store, name, left.min(Duration::from_secs(30)))?;
        if exp_exit(&r) != exit::RUNNING {
            let _ = child.wait();
            render::experiment(&r, json);
            return Ok(exp_exit(&r));
        }
        if std::time::Instant::now() >= deadline {
            render::experiment(&r, json);
            if !json {
                println!(
                    "next: prova exp wait {name} --max-wait {}",
                    max_wait.unwrap_or("9m")
                );
            }
            return Ok(exit::RUNNING);
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

fn init() -> Result<u8> {
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
    // Where harnesses look (researched 2026-11-24): `.agents/skills` serves
    // Codex, Copilot, Cursor, Gemini, Antigravity, OpenCode, Amp, Kimi Code,
    // pi, Goose, Devin, Roo, Zed, Factory and Warp; Claude Code, Cline and
    // Kimchi read only `.claude/skills`. Kiro and Junie read only their own
    // directory: written where the project already uses them.
    let mut dirs = vec![".agents/skills", ".claude/skills"];
    for (own, skills) in [(".kiro", ".kiro/skills"), (".junie", ".junie/skills")] {
        if root.join(own).is_dir() {
            dirs.push(skills);
        }
    }
    let mut failures: Vec<String> = Vec::new();
    let mut files = 0;
    for dir in &dirs {
        match write_skills(&root.join(dir), &names) {
            Ok(n) => files = n,
            Err(e) => failures.push(format!("{dir}: {e}")),
        }
    }
    println!(
        "installed {} skills ({files} files) into {}: {}",
        names.len(),
        dirs.join(", "),
        names.join(", ")
    );
    // AGENTS.md, read by nearly every harness, always; and the section in
    // instruction files that already exist and would otherwise keep a
    // harness from reading it: CLAUDE.md (Claude Code then ignores
    // AGENTS.md), GEMINI.md (Gemini CLI's default), and the files Zed reads
    // first. None of those is created: a new one would hide AGENTS.md.
    // Every target is processed; failures are reported with their path.
    let mut updated = Vec::new();
    let mut targets: Vec<(&str, bool)> = vec![("AGENTS.md", true)];
    for f in [
        "CLAUDE.md",
        ".claude/CLAUDE.md",
        "GEMINI.md",
        ".rules",
        ".cursorrules",
        ".windsurfrules",
        ".clinerules",
        ".github/copilot-instructions.md",
    ] {
        let path = root.join(f);
        if path.is_file() || path.is_symlink() {
            targets.push((f, false));
        }
    }
    for (f, create) in targets {
        match upsert_section(&root, &root.join(f), create) {
            Ok(Section::Changed) => updated.push(f),
            Ok(Section::Unchanged) => {}
            Ok(Section::Skipped(why)) => eprintln!("skipped {f}: {why}"),
            Err(e) => failures.push(format!("{f}: {e}")),
        }
    }
    if !updated.is_empty() {
        println!("updated the prova section in {}", updated.join(", "));
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
        match std::fs::write(&gi, format!("{ignore}{sep}.prova/\n")) {
            Ok(()) => println!("added .prova/ to .gitignore"),
            Err(e) => failures.push(format!(".gitignore: {e}")),
        }
    }
    if !root.join("prova.toml").exists() {
        println!("next: prova adopt   (proposes prova.toml for this project)");
    }
    if failures.is_empty() {
        return Ok(exit::PASSED);
    }
    for f in &failures {
        eprintln!("error: {f}");
    }
    eprintln!(
        "init finished with {} failure(s); every other file was done",
        failures.len()
    );
    Ok(exit::INFRA)
}

/// What `upsert_section` did to one instruction file.
enum Section {
    Changed,
    Unchanged,
    /// Left alone on purpose, with the reason.
    Skipped(String),
}

/// Puts the prova section into an instruction file: replaces it between
/// its markers, or appends it. A file that imports AGENTS.md (`@AGENTS.md`)
/// already has it. Returns whether the file changed.
///
/// Only a well-formed file (one begin, one end, begin first) is updated in
/// place; any other marker state is an error and the file stays untouched.
/// A symlink resolving outside `root` is never written through. The file's
/// line ending (LF or CRLF) is kept.
fn upsert_section(root: &Path, path: &Path, create: bool) -> Result<Section> {
    const BEGIN: &str = "<!-- prova:begin -->";
    const END: &str = "<!-- prova:end -->";
    let name = path
        .strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string();
    if path.is_symlink() {
        let root_real = root.canonicalize()?;
        match path.canonicalize() {
            Ok(real) if real.starts_with(&root_real) => {}
            Ok(real) => {
                return Ok(Section::Skipped(format!(
                    "symlink points outside the repository ({})",
                    real.display()
                )));
            }
            Err(_) => return Ok(Section::Skipped("dangling symlink".to_owned())),
        }
    }
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    if text.is_empty() && !create {
        return Ok(Section::Unchanged);
    }
    if text.lines().any(|l| l.trim() == "@AGENTS.md") {
        return Ok(Section::Unchanged);
    }
    let crlf = text.contains("\r\n");
    let eol = if crlf { "\r\n" } else { "\n" };
    let section = if crlf {
        skills::AGENTS_SECTION.replace('\n', "\r\n")
    } else {
        skills::AGENTS_SECTION.to_owned()
    };
    let broken = |what: &str| {
        anyhow::anyhow!("{name}: prova section markers are broken ({what}); fix them by hand")
    };
    let new = match (text.matches(BEGIN).count(), text.matches(END).count()) {
        (0, 0) if text.is_empty() => section,
        (0, 0) => {
            let mut t = text.clone();
            if !t.ends_with('\n') {
                t.push_str(eol);
            }
            t.push_str(eol);
            t.push_str(&section);
            t
        }
        (1, 1) => match (text.find(BEGIN), text.find(END)) {
            (Some(a), Some(b)) if a < b => {
                format!(
                    "{}{}{}",
                    &text[..a],
                    section.trim_end(),
                    &text[b + END.len()..]
                )
            }
            _ => return Err(broken("end before begin")),
        },
        (_, 0) => return Err(broken("begin without end")),
        (0, _) => return Err(broken("end without begin")),
        _ => return Err(broken("duplicate markers")),
    };
    if new == text {
        return Ok(Section::Unchanged);
    }
    std::fs::write(path, new)?;
    Ok(Section::Changed)
}

/// Whether a test prova names `calc.CalcTest::divides` is one a framework
/// filter selects. Filters use the framework's syntax: a Go regex
/// (`^TestAdd$`), Maven's `Class#method` with `*` and `+` (round 8), .NET's
/// `FullyQualifiedName~Name`, or a plain substring.
fn filter_matches(filter: &str, test: &str) -> bool {
    let f = filter
        .trim()
        .trim_start_matches("FullyQualifiedName~")
        .trim_start_matches("FullyQualifiedName=");
    // Maven: `Class#a+b` selects methods a and b of Class.
    if let Some((class, methods)) = f.split_once('#') {
        return methods
            .split('+')
            .any(|m| filter_matches(&format!("{class}::{m}"), test));
    }
    let plain = f.trim_start_matches('^').trim_end_matches('$');
    if plain.contains('*') {
        // A glob: every literal piece, in order.
        let mut rest = test;
        for piece in plain.split('*').filter(|p| !p.is_empty()) {
            match rest.find(piece) {
                Some(i) => rest = &rest[i + piece.len()..],
                None => return false,
            }
        }
        return true;
    }
    test.contains(plain)
        || test.rsplit("::").next().is_some_and(|leaf| leaf == plain)
        || test.replace("::", ".").contains(plain)
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
                // A test the filter skipped did not run: Vitest `-t` and
                // others report the rest as skipped (round 7).
                prova_core::model::EventKind::TestFinished { test, outcome, .. }
                    if outcome != Outcome::Skipped =>
                {
                    Some(test.display())
                }
                _ => None,
            })
            .collect();
        // The filter is the framework's own syntax: a Go regex (`^TestAdd$`)
        // is anchored to the test name, not to prova's full name (round 6).
        let matches = |t: &str| filter_matches(&filter, t);
        let unrelated: Vec<&String> = tests.iter().filter(|t| !matches(t)).collect();
        // The framework's own "nothing matched" beats a level diagnosis: an
        // opaque run with that message means the filter's syntax is wrong
        // for the framework, not the level (round 8: surefire with -q).
        let nothing_matched = s
            .output_tail
            .iter()
            .chain(s.notes.iter())
            .chain(s.detail.iter())
            .find(|l| {
                let l = l.to_lowercase();
                l.contains("no tests matching")
                    || l.contains("no test matches")
                    || l.contains("no tests ran")
                    || l.contains("no tests found")
                    || l.contains("no tests to run")
            })
            .map(|l| render::clean(l.trim()));
        let (ok, why) = if let (Some(line), true) = (&nothing_matched, tests.is_empty()) {
            (
                false,
                format!(
                    "filter {filter:?} matched no test; the framework says: {line}. Use the framework's own filter syntax (Maven: Class#method, .NET: FullyQualifiedName~Name)"
                ),
            )
        } else if s.level == prova_core::model::Level::Opaque {
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
            if p.alpha > 0.1 || p.min_effect < 0.01 {
                eprintln!(
                    "warning: alpha {} and minimum effect {:.1}% make a lucky ACCEPT likely; every verdict will say so",
                    p.alpha,
                    p.min_effect * 100.0
                );
            }
            warn_unfrozen_oracle_paths(&p, &root);
            experiment::create(&store, &name, &hypothesis, &p, &cwd, base)?;
            // The baseline as a commit, not only a ref name (round 9).
            let sha = experiment::variant(&store, &name, "baseline")?
                .commit_sha
                .map(|s| format!(" ({})", &s[..s.len().min(12)]))
                .unwrap_or_default();
            println!(
                "experiment {name} created (protocol {}), baseline = {baseline}{sha}",
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
            for f in &c.no_specs {
                // The file may be gone from the tree: match the path as
                // given when it cannot be resolved.
                let rel = spec_files(std::slice::from_ref(f), &root)
                    .ok()
                    .and_then(|v| v.into_iter().next().map(|s| s.path))
                    .unwrap_or_else(|| f.to_string_lossy().into_owned());
                let before = p.specs.len();
                p.specs.retain(|s| s.path != rel);
                if p.specs.len() == before {
                    bail!(
                        "{rel} is not a specification file of {name} (registered: {})",
                        p.specs
                            .iter()
                            .map(|s| s.path.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
            }
            for f in spec_files(&c.specs, &root)? {
                p.specs.retain(|s| s.path != f.path);
                p.specs.push(f);
            }
            warn_unfrozen_oracle_paths(&p, &root);
            if c.oracle.is_some() {
                p.oracle = c.oracle;
            }
            if !c.scope.is_empty() {
                p.scope = c.scope;
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
            max_wait,
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
            exp_start(
                &store,
                &name,
                &candidate,
                against.as_deref(),
                detach,
                max_wait.as_deref(),
                json,
            )
        }
        ExpCmd::Revalidate {
            name,
            detach,
            max_wait,
            json,
        } => {
            let e = experiment::get(&store, &name)?;
            if e.best == "baseline" {
                bail!("nothing to revalidate: no candidate has been kept");
            }
            exp_start(
                &store,
                &name,
                &e.best,
                Some("baseline"),
                detach,
                max_wait.as_deref(),
                json,
            )
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
            Ok(exp_status_exit(&r))
        }
        ExpCmd::Best {
            name,
            variant,
            reason,
        } => {
            let from = experiment::reset_best(&store, &name, &variant, &reason)?;
            println!("best of {name}: {from} -> {variant} (recorded with the decisions)");
            println!("next: prova exp candidate {name}");
            Ok(exit::PASSED)
        }
        ExpCmd::Cancel { name } => {
            let id = experiment::latest_comparison(&store, &name)?
                .ok_or_else(|| anyhow!("no comparison to cancel"))?;
            // Nothing to stop when the latest comparison already ended
            // (round 10 said it would stop "after the current trial").
            let status = experiment::report(&store, &name)?
                .comparison
                .map(|c| c.status)
                .unwrap_or_default();
            if status != "running" {
                println!("nothing to cancel: comparison {id} already {status}");
                return Ok(exit::PASSED);
            }
            experiment::request_cancel(&store, &id)?;
            println!("cancel requested for comparison {id}; it stops after the current trial");
            Ok(exit::PASSED)
        }
        ExpCmd::Log { name, entry, tail } => {
            let Some(c) = experiment::latest_comparison(&store, &name)? else {
                bail!("experiment {name} has no comparison yet");
            };
            let kept = prova_core::output::comparison_excerpts(&store, &c)?;
            let shown: Vec<_> = kept
                .iter()
                .filter(|(n, _)| entry.as_deref().is_none_or(|e| e == n))
                .collect();
            if shown.is_empty() {
                println!(
                    "nothing kept for comparison {c}: logs are kept for the oracle and for what failed"
                );
            }
            for (n, e) in shown {
                // Trials are kept by number: say what the number is.
                if n.chars().all(|ch| ch.is_ascii_digit()) {
                    println!("== trial {n}");
                } else {
                    println!("== {n}");
                }
                if let Some(t) = tail {
                    for l in e.tail(t) {
                        println!("{l}");
                    }
                    continue;
                }
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
            // A benchmark that throws can vanish while the command exits 0
            // (Vitest bench does, round 7): never let wall time alone pass
            // for a benchmark result.
            if metrics.is_empty() {
                eprintln!(
                    "warning: no benchmark result was read, only the wall time; if this command runs benchmarks, they may have failed without a non-zero exit"
                );
                for l in text
                    .lines()
                    .filter(|l| {
                        let t = l.trim_start();
                        t.contains("Error") || t.starts_with("FAIL") || t.contains("Traceback")
                    })
                    .take(3)
                {
                    eprintln!("  {}", render::clean(l.trim()));
                }
            }
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
                    "next: prova exp new <name> --hypothesis \"<FILL IN: what you expect and why>\" --metric <one of these> --better lower|higher -- {}",
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

/// A mistake in how prova was asked: exit 2.
#[derive(Debug)]
struct UsageError(String);

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UsageError {}

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
    // `COMPLETE=<shell> prova ...`: the shell asking for completions.
    clap_complete::CompleteEnv::with_factory(Cli::command).complete();
    let cli = Cli::parse();
    match dispatch(cli.cmd) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("prova: error: {e:#}");
            // A mistake in how prova was asked is exit 2, not an
            // infrastructure failure (round 12).
            let usage = e.downcast_ref::<UsageError>().is_some()
                || e.downcast_ref::<config::ConfigError>()
                    .is_some_and(config::ConfigError::is_usage);
            ExitCode::from(if usage { exit::USAGE } else { exit::INFRA })
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
                // A one-line excerpt cut with "..." is the framework's own
                // short summary (pytest -q), not the test's output (round 9).
                if t.trim().lines().count() <= 1 && t.trim_end().ends_with("...") {
                    println!(
                        "note: this is the framework's shortened summary line, not the test's own output; `prova output {id}` has the run's output, and a verbose suite (`prova adopt --write`) keeps the full failure"
                    );
                }
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
        Cmd::Completions { shell } => {
            // The script is what `COMPLETE=<shell> prova` prints.
            let status = std::process::Command::new(std::env::current_exe()?)
                .env("COMPLETE", &shell)
                .status()?;
            Ok(if status.success() {
                exit::PASSED
            } else {
                exit::INFRA
            })
        }
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
            repeat,
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
                "--until",
                "--max-wait",
                "--repeat",
            ];
            if let Some(f) = command
                .iter()
                .find(|a| OWN_FLAGS.contains(&a.split('=').next().unwrap_or(a)))
            {
                // Refuse instead of running a command we already know is
                // wrong (round 11: `make test --until first-failure` ran and
                // left a bogus failed run in history).
                bail!(
                    "{f} comes after `--`, so it would go to the command, not to prova: put prova's flags before `--` (to pass it to the command on purpose, wrap it: prova run -- sh -c '...')"
                );
            }
            let (suite_name, mut spec, run_cwd) = if !command.is_empty() {
                if suite.is_some() {
                    bail!("give either a suite name or a command after --, not both");
                }
                // --select needs a suite's select template (round 9: the
                // old message talked about a suite nobody gave).
                if select.is_some() {
                    let with: Vec<&String> = cfg
                        .suite
                        .iter()
                        .filter(|(_, s)| s.select.is_some())
                        .map(|(n, _)| n)
                        .collect();
                    match with.first() {
                        Some(n) => return Err(UsageError(format!(
                            "--select runs one test through a suite's select template, not a command after --: prova run {n} --select <test>"
                        )).into()),
                        None => return Err(UsageError(
                            "--select runs one test through a suite's select template; there is none yet: `prova adopt --write` proposes one".into()
                        ).into()),
                    }
                }
                let rel = cwd
                    .strip_prefix(&root)
                    .unwrap_or(Path::new("."))
                    .to_string_lossy()
                    .into_owned();
                let rel = if rel.is_empty() { ".".to_owned() } else { rel };
                // The same command as a configured suite runs without the
                // suite's injections and settings: point to it (round 7).
                let joined = command.join(" ");
                if let Some((name, _)) = cfg
                    .suite
                    .iter()
                    .find(|(_, s)| s.run.trim() == joined.trim())
                {
                    eprintln!(
                        "prova: note: suite `{name}` in prova.toml runs this command with its own settings (more detail): prova run {name}"
                    );
                }
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
            let request = NewRunRequest {
                suite: suite_name,
                spec,
                cwd: run_cwd,
                extra_env: cfg.env.clone(),
            };
            if let Some(n) = repeat {
                // Each repeat runs to its end: `--until done` says the same
                // (the skills teach it everywhere, round 7); others do not fit.
                if until.is_some_and(|u| !matches!(u, UntilArg::Done)) {
                    bail!("--repeat runs each run to its end; use it alone or with --until done");
                }
                return repeat_runs(&store, &request, n, max_wait.as_deref(), json);
            }
            let id = engine::create_run(&store, &request)?;
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
            let (code, done) = attached(&store, &id, json)?;
            // On a stall the run goes on, detached; wait for the supervisor
            // only when the run ended.
            if done {
                let _ = child.wait();
            }
            Ok(code)
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
        Cmd::Forget { run, reason } => {
            let (store, _, _) = open_here()?;
            let id = store.resolve_run(&run)?;
            store.forget(&id, &reason)?;
            println!("run {id} no longer counts in any test's history ({reason})");
            Ok(exit::PASSED)
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
                // An aborted run's command may outlive its supervisor.
                let killed = engine::kill_leftovers(&store, &id)?;
                if killed > 0 {
                    println!(
                        "run {id} already {}; killed {killed} process(es) it left running",
                        row.status.as_str()
                    );
                } else {
                    println!("run {id} already {}", row.status.as_str());
                }
                return Ok(exit::PASSED);
            }
            store.request_cancel(&id)?;
            // Most runs end within a second or two: wait that long, so the
            // next `status` does not still say RUNNING (round 7).
            let s = engine::wait(&store, &id, Until::Done, 0, Duration::from_secs(5))?;
            if s.reason == WaitReason::Done {
                render::summary(&s, false);
                return Ok(exit::PASSED);
            }
            println!("cancel requested for run {id}; it is still stopping");
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
                    render::runs(&runs, &engine::unverified_runs(&store, &runs)?, false);
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
                    render::runs(&runs, &engine::unverified_runs(&store, &runs)?, json);
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
            // Exact names win over partial ones (round 10: 16 matches for a
            // name that is exact in every suite, next to an old renamed one).
            let found: Vec<_> = {
                let exact: Vec<_> = found.iter().filter(|t| t.1 == test).cloned().collect();
                if exact.is_empty() { found } else { exact }
            };
            let exact: Vec<_> = found.iter().filter(|t| t.1 == test).collect();
            // Same name in several suites: take the one that ran most recently.
            let displays: std::collections::BTreeSet<&str> =
                found.iter().map(|t| t.1.as_str()).collect();
            let pick = match (exact.as_slice(), found.as_slice()) {
                ([one], _) => one.0.clone(),
                ([], [one]) => one.0.clone(),
                (_, [_, ..]) if displays.len() == 1 => analysis::most_recent(&store, &found)?,
                // Several names, one test: they share one history (round 6).
                (_, [first, ..])
                    if {
                        let shared = analysis::shared_ids(&store, &first.0)?;
                        found.iter().all(|f| shared.contains(&f.0))
                    } =>
                {
                    analysis::most_recent(&store, &found)?
                }
                (_, []) => {
                    return Err(UsageError(format!(
                        "no test matching {test:?}: prova knows a test once a run showed it individually (`prova history \"\"` lists them)"
                    ))
                    .into());
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
                    "{} tests by what they used (ADR 0025): small {}  medium {}  large {}  unknown {}",
                    d.total, d.small, d.medium, d.large, d.unknown
                );
                if let Some(w) = &d.warning {
                    println!("warning: {w}");
                }
                if d.unknown > 0 {
                    println!(
                        "unknown: {} seen small in fewer than {} runs so far; the others were never seen on their own (one test running, or its own process) and their runs were not sampled in time to tell (their output came all at once). Sizes are a lower bound: prova samples every 0.5 s and cannot see sleeps",
                        d.small_so_far,
                        analysis::SMALL_AFTER
                    );
                }
                for (t, label) in d.larger.iter().take(20) {
                    println!("  {}: {}", render::clean(t), render::clean(label));
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
                    ("changed, but known flaky", &d.flaky),
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
        Cmd::Init { .. } => init(),
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
            experiment::work(
                &store,
                &comparison,
                &prova_adapters::bench::parse,
                &prova_adapters::bench::counts,
                &prova_adapters::recognize::passed_tests,
            )?;
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
/// `prova run --repeat N`: the same run N times in a row, each to its end,
/// then each test's tally, so confirming a flaky test needs no shell loop
/// (round 6). One budget for all of them; when it ends, the command to run
/// the remaining repeats is printed.
fn repeat_runs(
    store: &Store,
    request: &NewRunRequest,
    n: u32,
    max_wait: Option<&str>,
    json: bool,
) -> Result<u8> {
    let budget = wait_budget(UntilArg::Done, max_wait)?;
    let start = std::time::Instant::now();
    let exe = std::env::current_exe()?;
    let mut tally: std::collections::BTreeMap<String, (u32, u32)> = Default::default();
    let mut runs = Vec::new();
    let mut any_failed = false;
    // Runs that ran no test, and runs that failed with no failing test:
    // never "every test passed" (round 13: five runs whose selection matched
    // nothing were tallied as all green).
    let (mut no_tests, mut failed_otherwise) = (0u32, 0u32);
    for i in 1..=n {
        let left = budget.saturating_sub(start.elapsed());
        if left.is_zero() {
            break;
        }
        let id = engine::create_run(store, request)?;
        let mut child =
            engine::spawn_supervisor(store, &exe, &id).context("starting supervisor")?;
        let s = wait_until(store, &id, UntilArg::Done, 0, left)?;
        let _ = child.wait();
        if s.reason != WaitReason::Done {
            if !json {
                println!("run {i}/{n} {id}: still running when the budget ended");
                println!(
                    "next: prova wait {id} --until done, then run the remaining {} repeat(s)",
                    n - i
                );
            }
            return Ok(exit::RUNNING);
        }
        any_failed |= s.outcome != Some(Outcome::Passed);
        let ran = s.counts.results();
        if ran == 0 {
            no_tests += 1;
        } else if s.outcome != Some(Outcome::Passed) && s.failures.is_empty() {
            failed_otherwise += 1;
        }
        for e in store.events_after(&id, 0)? {
            if let prova_core::model::EventKind::TestFinished { test, outcome, .. } = e.kind {
                let t = tally.entry(test.display()).or_default();
                if outcome.is_failure() {
                    t.1 += 1;
                } else if outcome == Outcome::Passed {
                    t.0 += 1;
                }
            }
        }
        if !json {
            let failing: Vec<&str> = s.failures.iter().map(|f| f.test.as_str()).collect();
            let why = if !failing.is_empty() {
                format!(" ({})", failing.join(", "))
            } else if ran == 0 {
                " (no test ran)".to_owned()
            } else if let (Some(Outcome::Failed | Outcome::Errored), Some(d)) =
                (s.outcome, &s.detail)
            {
                format!(" ({})", render::clean(d))
            } else {
                String::new()
            };
            eprintln!(
                "run {i}/{n} {id}: {}{why}",
                s.outcome.map(|o| o.as_str()).unwrap_or("?"),
            );
        }
        runs.push(serde_json::json!({ "run": id, "outcome": s.outcome }));
    }
    let done = runs.len() as u32;
    let mixed: Vec<(&String, &(u32, u32))> = tally
        .iter()
        .filter(|(_, (p, f))| *p > 0 && *f > 0)
        .collect();
    let always: Vec<&String> = tally
        .iter()
        .filter(|(_, (p, f))| *p == 0 && *f > 0)
        .map(|(t, _)| t)
        .collect();
    if json {
        let tests: Vec<_> = tally
            .iter()
            .map(|(t, (p, f))| serde_json::json!({ "test": t, "passed": p, "failed": f }))
            .collect();
        println!(
            "{}",
            serde_json::json!({ "runs": runs, "repeats": done, "tests": tests })
        );
    } else {
        println!("{done} run(s) of the same code:");
        for (t, (p, f)) in &mixed {
            println!("  FLAKY {}: failed {f} of {}", render::clean(t), p + f);
        }
        for t in &always {
            println!("  FAIL {}: failed every time", render::clean(t));
        }
        let steady = tally.values().filter(|(p, f)| *f == 0 && *p > 0).count();
        if no_tests > 0 {
            println!("  {no_tests} of {done} run(s) ran no test (did the selection match one?)");
        }
        if failed_otherwise > 0 {
            println!(
                "  {failed_otherwise} of {done} run(s) failed with no failing test (see each run's status)"
            );
        }
        if tally.is_empty() {
            // Nothing to say about tests.
        } else if mixed.is_empty() && always.is_empty() && no_tests == 0 && failed_otherwise == 0 {
            println!("  every test passed every time");
        } else if mixed.is_empty() && always.is_empty() {
            println!("  {steady} test(s) passed whenever they ran");
        } else if steady > 0 {
            println!("  {steady} other test(s) passed every time");
        }
        // A known flaky test that passed every time here proves little
        // (round 9: "every test passed every time" for a 22% flake).
        for f in analysis::flaky(store, None, 50)? {
            if let Some((p, 0)) = tally.get(&f.display)
                && *p > 0
            {
                println!(
                    "  note: {} passed {p} of {p} here, but history knows it as flaky ({:.0}% of {} runs failed): {p} passes are weak evidence",
                    render::clean(&f.display),
                    f.failure_rate * 100.0,
                    f.runs
                );
            }
        }
        if done < n {
            println!(
                "budget ended after {done} of {n}; next: run the remaining {} with --repeat {}",
                n - done,
                n - done
            );
        }
    }
    Ok(if any_failed {
        exit::FAILED
    } else if done < n {
        exit::RUNNING
    } else {
        exit::PASSED
    })
}

/// Waits for `until` within `budget` (ADR 0019). Short waits for the first
/// failure return as they are; a wait for the result or a stall shows
/// progress on stderr for the person watching: every new failure, slow
/// test and stall as it happens, and a live line on a terminal (one line
/// every 30 s in a log).
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
    let mut live = render::Live::new();
    loop {
        let left = budget.saturating_sub(start.elapsed());
        if left.is_zero() {
            live.clear();
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
            WaitReason::Done => {
                live.clear();
                return Ok(s);
            }
            // A stall ends any long wait: waiting out the budget on a hung
            // run helps nobody (round 6); the summary says to cancel.
            // (Not once a cancel was asked for: then the end is coming.)
            WaitReason::Stall if !store.cancel_requested(id)? => {
                live.clear();
                return Ok(s);
            }
            WaitReason::Failure | WaitReason::Slow | WaitReason::Stall => {
                live.clear();
                render::progress(&s, &mut reported)
            }
            WaitReason::Running => {}
        }
        // A person at a terminal sees the run move; a log gets a line
        // every 30 s.
        if live.on() {
            let last = engine::output_tail(store, id, 1);
            live.show(&s, last.last().map(String::as_str));
        } else if beat.elapsed() >= Duration::from_secs(30) {
            beat = std::time::Instant::now();
            render::heartbeat(&s);
        }
    }
}

/// Follows a run in the foreground; returns its exit code, and whether it
/// ended (a stall returns while it goes on).
fn attached(store: &Store, id: &str, json: bool) -> Result<(u8, bool)> {
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
    let mut live = render::Live::new();
    let mut beat = std::time::Instant::now();
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
                live.clear();
                render::summary(&s, json);
                return Ok((exit_code(&s), true));
            }
            // A stall ends the attached wait too (ADR 0004: no long waits;
            // round 7: a hang blocked a bare `prova run -- cmd` forever).
            // The run goes on; the summary says to cancel it.
            WaitReason::Stall if !cancel_sent => {
                live.clear();
                render::summary(&s, json);
                return Ok((exit_code(&s), false));
            }
            WaitReason::Failure | WaitReason::Stall | WaitReason::Slow if !json => {
                live.clear();
                render::progress(&s, &mut reported)
            }
            _ => {}
        }
        // Progress while it runs (round: a 50 s `prova run -- make test`
        // that passed showed nothing until the end): a live line on a
        // terminal, a line every 30 s in a log.
        if !json {
            if live.on() {
                let last = engine::output_tail(store, id, 1);
                live.show(&s, last.last().map(String::as_str));
            } else if beat.elapsed() >= Duration::from_secs(30) {
                beat = std::time::Instant::now();
                render::heartbeat(&s);
            }
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
mod filter_tests {
    use super::filter_matches;

    #[test]
    fn framework_filters_match_prova_names() {
        let t = "calc.CalcTest::divides";
        assert!(filter_matches("CalcTest#divides", t));
        assert!(filter_matches("CalcTest#div*", t));
        assert!(filter_matches("CalcTest#adds+divides", t));
        assert!(!filter_matches("CalcTest#adds", t));
        assert!(filter_matches("^divides$", t));
        assert!(filter_matches(
            "FullyQualifiedName~Calc.CalcTests.Divides",
            "Calc.CalcTests::Divides"
        ));
        assert!(filter_matches("calc.CalcTest.divides", t));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn usage_exit_code_matches_clap() {
        assert_eq!(super::exit::USAGE, 2);
    }
}

#[cfg(test)]
mod help_tests {
    use super::Cli;
    use clap::CommandFactory;

    /// Every visible option and argument says what it is, and every
    /// visible command shows examples (owner: "`prova output` is super
    /// hard, idk what are the options").
    #[test]
    fn every_option_is_described_and_every_command_has_examples() {
        fn walk(cmd: &clap::Command, path: &str, missing: &mut Vec<String>) {
            for a in cmd.get_arguments() {
                let id = a.get_id().as_str();
                if a.is_hide_set() || id == "help" || id == "version" {
                    continue;
                }
                if a.get_help().is_none() {
                    missing.push(format!("{path} {id}: no description"));
                }
            }
            if cmd.get_after_help().is_none() && cmd.get_subcommands().next().is_none() {
                missing.push(format!("{path}: no examples"));
            }
            for sub in cmd.get_subcommands() {
                if sub.is_hide_set() || sub.get_name() == "help" {
                    continue;
                }
                walk(sub, &format!("{path} {}", sub.get_name()), missing);
            }
        }
        let mut missing = Vec::new();
        walk(&Cli::command(), "prova", &mut missing);
        assert!(missing.is_empty(), "{missing:#?}");
    }
}
