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
  prova history tests::divides             new failure, pre-existing, or flaky?
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

#[derive(Subcommand)]
enum QuarantineCmd {
    /// Quarantine a test prova has seen, with the reason it is known flaky.
    #[command(after_help = "\
Example:
  prova quarantine add tests::flaky_clock --reason \"clock tick, issue 42\"")]
    Add {
        /// The test's full name, or a substring that matches one test.
        #[arg(add = ArgValueCompleter::new(complete_tests))]
        test: String,
        /// Why it is quarantined (shown in every run that touches it).
        #[arg(long)]
        reason: String,
        /// Days until the quarantine expires and the test counts again.
        #[arg(long, value_name = "DAYS", default_value_t = prova_core::quarantine::DEFAULT_DAYS,
              value_parser = clap::value_parser!(u32).range(1..=prova_core::quarantine::MAX_DAYS as i64))]
        days: u32,
        /// Where the test lives, when several places share its name: a Rust test binary
        /// (crate), a Go package, a pytest or Vitest file (`tests/test_calc.py`, `ex/a`)
        /// (e.g. two crates' `tests::flaky`).
        #[arg(long = "in", value_name = "FILE")]
        in_file: Option<String>,
    },
    /// List the quarantined tests.
    #[command(after_help = "\
Examples:
  prova quarantine list
  prova quarantine list --all --json")]
    List {
        /// Include removed quarantines.
        #[arg(long)]
        all: bool,
        /// JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// End a quarantine: the test counts again.
    #[command(after_help = "\
Example:
  prova quarantine remove tests::flaky_clock")]
    Remove {
        /// The quarantined test's name, or a substring that matches one.
        #[arg(add = ArgValueCompleter::new(complete_quarantined))]
        test: String,
        /// Where the test lives, when it is quarantined in several places
        /// (the place `quarantine list` shows).
        #[arg(long = "in", value_name = "FILE")]
        in_file: Option<String>,
    },
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
  prova run unit --until first-failure       back at the first failure, a slow test or a stall (30s budget)
  prova run unit --until done                the result (waits up to 9m, then prints `next:`)
  prova run unit --select tests::divides     rerun one test (the suite's `select` template)
  prova run unit --repeat 10                 is it flaky? each test's pass/fail tally
  prova run unit --detach                    start, print the run id, return; then `prova wait`")]
    Run {
        /// Suite name from prova.toml (omit it and give a command after `--`).
        #[arg(add = ArgValueCompleter::new(complete_suites))]
        suite: Option<String>,
        /// Run only tests matching this filter (uses the suite's `select`).
        #[arg(long, value_name = "TEST", add = ArgValueCompleter::new(complete_tests))]
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
        /// Default: the suite's `stall_after`, else by size (small 20s,
        /// medium 60s, large 5m), else 20s.
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
        #[arg(long, value_name = "TEST", add = ArgValueCompleter::new(complete_tests))]
        test: Option<String>,
        /// JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// History and flake statistics of tests matching a name.
    #[command(after_help = "\
Examples:
  prova history tests::divides          one test: outcomes, code versions, size, flakiness
  prova history divides --suite unit    only runs of one suite
  prova history tests::flaky --in geo   the test of that name in one binary, package or file
  prova history \"\"                      every test prova knows")]
    History {
        /// Substring of the test's full name.
        #[arg(add = ArgValueCompleter::new(complete_tests))]
        test: String,
        /// Only runs of this suite.
        #[arg(long, add = ArgValueCompleter::new(complete_suites))]
        suite: Option<String>,
        /// Only the test of this place: a Rust test binary (crate), a Go
        /// package, a pytest or Vitest file.
        #[arg(long = "in", value_name = "PLACE")]
        in_place: Option<String>,
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
        #[arg(long, value_name = "RUNS", default_value_t = 50)]
        window: usize,
        /// JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Test sizes from what each test used (processes, network), with the reason.
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
    /// The slowest tests from history: p50 and p95, worst first.
    #[command(after_help = "\
Examples:
  prova slowest                 the 10 slowest tests of this repository
  prova slowest -n 30           more of them
  prova slowest --suite unit    one suite, as JSON with --json")]
    Slowest {
        /// Only runs of this suite.
        #[arg(long, add = ArgValueCompleter::new(complete_suites))]
        suite: Option<String>,
        /// How many tests to list.
        #[arg(short = 'n', long, value_name = "N", default_value_t = 10)]
        count: usize,
        /// JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Mark known-flaky tests, in the open: named in every run, never passing one.
    #[command(after_help = "\
Examples:
  prova quarantine add tests::flaky_clock --reason \"clock tick, issue 42\"
  prova quarantine add tests::flaky_clock --reason \"still flaky\" --days 30
  prova quarantine list           what is quarantined now, and until when
  prova quarantine list --all     including removed ones
  prova quarantine remove tests::flaky_clock

A quarantined test still runs and keeps its history, and every summary lists it
with its result. It never turns a failed run green: when every failing test is
quarantined the run says so, and still fails. A quarantine expires.
It is kept in this machine's prova store (PROVA_HOME), not in the repository:
CI and teammates do not see it; each quarantines in its own store.")]
    Quarantine {
        #[command(subcommand)]
        action: QuarantineCmd,
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
Examples:
  prova exp wait speed --max-wait 5m
  prova exp wait speed --max-wait 1h     as long as the comparison needs
The 9 minute limit is only the foreground `exp run` budget: `exp wait` takes
any --max-wait, so one command (in the background if your tool calls are
short) replaces a polling loop.
Exit codes: 0 ACCEPT (or no comparison yet), 1 REJECT or any other verdict
that is not an accept, 3 cancelled, 4 the comparison failed, 5 still running:
wait again.")]
    Wait {
        /// Experiment name.
        #[arg(add = ArgValueCompleter::new(complete_experiments))]
        name: String,
        /// Wait at most this long (any length, e.g. 1h).
        #[arg(long, value_name = "DURATION", default_value = "30s")]
        max_wait: String,
        /// JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Show the experiment, latest comparison and decision log. Read the
    /// frozen protocol, the verdict and its statistics from `--json`, never
    /// from the database or the text.
    #[command(after_help = "\
Examples:
  prova exp status speed
  prova exp status speed --json
  prova exp status speed --comparison 6ddda6cb   an older comparison
`prova exp protocol` changes the protocol (before the first trial); it is
not how to read it.
Exit codes: 0 ACCEPT, no comparison yet, or a REJECT after a candidate was
accepted as best; 1 REJECT with no accepted best; 3 cancelled; 4 the
comparison failed; 5 still running.")]
    Status {
        /// Experiment name.
        #[arg(add = ArgValueCompleter::new(complete_experiments))]
        name: String,
        /// Show this comparison instead of the latest (an id from the
        /// decision log).
        #[arg(long, value_name = "ID")]
        comparison: Option<String>,
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
    /// setup, the oracle), or in the one `--comparison` names.
    #[command(after_help = "\
Examples:
  prova exp log speed                    every kept log
  prova exp log speed oracle-candidate   one of them
  prova exp log speed 3 --tail 20        trial 3, last 20 lines
  prova exp log speed --comparison 6ddda6cb   an older comparison")]
    Log {
        /// Experiment name.
        #[arg(add = ArgValueCompleter::new(complete_experiments))]
        name: String,
        /// One log: a trial number, setup-baseline, oracle-candidate, ...
        entry: Option<String>,
        /// Only the last N kept lines of each log.
        #[arg(long)]
        tail: Option<usize>,
        /// Read this comparison instead of the latest (an id from the
        /// decision log).
        #[arg(long, value_name = "ID")]
        comparison: Option<String>,
    },
    /// Print the evidence behind the verdicts as JSON (`prova.exp-export.v1`,
    /// schema in schemas/exp-export.v1.json): the protocol, every
    /// comparison with its trials (warmups and failed ones included) and
    /// their samples, the decisions. Read-only; `partial` while a comparison
    /// runs.
    #[command(after_help = "\
Examples:
  prova exp export speed                          everything, to stdout
  prova exp export speed --comparison 6ddda6cb --output evidence.json")]
    Export {
        /// Experiment name.
        #[arg(add = ArgValueCompleter::new(complete_experiments))]
        name: String,
        /// Only this comparison (an id from the decision log).
        #[arg(long, value_name = "ID")]
        comparison: Option<String>,
        /// Write the JSON to this file instead of stdout.
        #[arg(long, value_name = "FILE")]
        output: Option<PathBuf>,
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

/// Test names prova has seen, by prefix (round 14: typos and ambiguous
/// names happen exactly where test names are typed).
fn complete_tests(current: &std::ffi::OsStr) -> Vec<CompletionCandidate> {
    let prefix = current.to_string_lossy();
    let Some(store) = existing_store() else {
        return Vec::new();
    };
    let Ok(mut stmt) = store.conn().prepare(
        "SELECT DISTINCT display FROM tests WHERE substr(display, 1, length(?1)) = ?1
          ORDER BY display LIMIT 200",
    ) else {
        return Vec::new();
    };
    stmt.query_map([prefix.as_ref()], |r| r.get::<_, String>(0))
        .map(|rows| {
            rows.filter_map(Result::ok)
                .map(CompletionCandidate::new)
                .collect()
        })
        .unwrap_or_default()
}

fn complete_quarantined(current: &std::ffi::OsStr) -> Vec<CompletionCandidate> {
    let prefix = current.to_string_lossy();
    let Some(store) = existing_store() else {
        return Vec::new();
    };
    prova_core::quarantine::list(&store, false)
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.test.starts_with(prefix.as_ref()))
        .map(|e| CompletionCandidate::new(e.test).help(Some(e.reason.into())))
        .collect()
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
            render::experiment_with_next(
                &r,
                json,
                Some(format!(
                    "prova exp wait {name} --max-wait {}",
                    max_wait.unwrap_or("9m")
                )),
            );
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
    let mut created = Vec::new();
    for (f, create) in targets {
        let existed = root.join(f).exists();
        match upsert_section(&root, &root.join(f), create) {
            // "updated" a file init had just created (round 14).
            Ok(Section::Changed) if !existed => created.push(f),
            Ok(Section::Changed) => updated.push(f),
            Ok(Section::Unchanged) => {}
            Ok(Section::Skipped(why)) => eprintln!("skipped {f}: {why}"),
            Err(e) => failures.push(format!("{f}: {e}")),
        }
    }
    if !created.is_empty() {
        println!("created {} with a prova section", created.join(", "));
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
    // A pytest node id names the test the way prova prints it, dotted.
    let filter = &prova_core::config::pytest_prova_name(filter);
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

/// The wrapper this project's earlier runs put in front of the test command
/// (`./dev`, `./dev "dotnet test"`, `nix develop -c`): the most frequent
/// over the recent runs, argv or a suite's shell command, the newest first
/// among equals (round 16: the newest run alone lost it).
fn earlier_wrapper(store: &Store, root: &Path) -> Option<prova_adapters::adopt::Wrapper> {
    let mut seen: Vec<(prova_adapters::adopt::Wrapper, usize)> = Vec::new();
    for r in store.recent_runs(300).ok()? {
        let found = learnable_wrapper(&r.spec["command"], root);
        if let Some(w) = found {
            match seen.iter_mut().find(|(x, _)| *x == w) {
                Some((_, n)) => *n += 1,
                None => seen.push((w, 1)),
            }
        }
    }
    // `max_by_key` keeps the last of equals: the list is newest first.
    seen.into_iter()
        .rev()
        .max_by_key(|(_, n)| *n)
        .map(|(w, _)| w)
}

/// The wrapper an earlier run's command shows, if adopt may propose it. The
/// run itself may be anything (`./dev make test`), but the wrapper's program
/// must exist (round 18: `./ci.sh` was proposed after it was deleted). Whether
/// the wrapper "did more than run tests" no longer matters: a quarantine never
/// turns a failed run green (ADR 0034).
fn learnable_wrapper(
    command: &serde_json::Value,
    root: &Path,
) -> Option<prova_adapters::adopt::Wrapper> {
    let found = match (command["argv"].as_array(), command["shell"].as_str()) {
        (Some(argv), _) => {
            let argv: Vec<String> = argv
                .iter()
                .filter_map(|w| w.as_str().map(str::to_owned))
                .collect();
            prova_adapters::adopt::wrapper_form(&argv)
        }
        (None, Some(cmd)) => prova_adapters::adopt::wrapper_of_shell(cmd),
        _ => return None,
    };
    let found = found?;
    let program = found.prefix.split_whitespace().next()?;
    program_exists(program, root).then_some(found)
}

/// Whether `program` can start: a path that is a file, or a name on PATH.
fn program_exists(program: &str, root: &Path) -> bool {
    if program.contains('/') {
        return root.join(program).is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|d| d.join(program).is_file()))
}

/// What the framework named in a filter's syntax, for the framework the
/// command runs.
fn filter_syntax_advice(command: &str) -> &'static str {
    let c = command;
    if c.contains("pytest") {
        "pytest selects with a `-k` expression (a substring of the test name) or a node id (tests/test_x.py::test_name)"
    } else if c.contains("vitest") || c.contains("jest") {
        "Vitest and Jest select with `-t <name pattern>`"
    } else if c.contains("go test") || c.contains("gotestsum") {
        "Go selects with `-run <regex>` (anchor it: ^TestName$)"
    } else if c.contains("mvn") || c.contains("gradle") {
        "Maven selects with `-Dtest=Class#method` (Gradle: `--tests Class.method`)"
    } else if c.contains("dotnet") {
        ".NET selects with `--filter FullyQualifiedName~Name`"
    } else if c.contains("cargo") {
        "cargo selects with a substring of the test path"
    } else {
        "use the framework's own filter syntax"
    }
}

/// Why a run that ran no test failed on its own: the command could not
/// start (126/127), or it exited nonzero and printed an error. None when it
/// ran fine (exit 0), so the level is the issue.
fn run_failure_cause(s: &Summary) -> Option<String> {
    match s.exit_code {
        Some(126 | 127) => s
            .detail
            .clone()
            .or_else(|| Some(format!("exit {}", s.exit_code.unwrap_or_default()))),
        Some(code) if code != 0 => {
            let error = s.output_tail.iter().map(|l| l.trim()).find(|l| {
                let l = l.to_lowercase();
                [
                    "error",
                    "no module named",
                    "not found",
                    "no such file",
                    "permission denied",
                    "cannot",
                ]
                .iter()
                .any(|w| l.contains(w))
            });
            let last = s
                .output_tail
                .iter()
                .map(|l| l.trim())
                .rfind(|l| !l.is_empty());
            Some(match error.or(last) {
                Some(line) => format!("exit {code}: {}", render::clean(line)),
                None => format!("exit {code} with no output"),
            })
        }
        _ => None,
    }
}

/// pytest's own complaint about a `-k` expression it cannot parse.
fn rejected_k_expression(lines: &[&String]) -> Option<String> {
    lines
        .iter()
        .find(|l| l.contains("Wrong expression passed to '-k'"))
        .map(|l| render::clean(l.trim()))
}

/// A line in which a framework says it ran no test (Surefire, pytest, Jest,
/// dotnet).
fn names_zero_tests(line: &str) -> bool {
    let l = line.to_lowercase();
    l.contains("tests run: 0,")
        || l.contains("collected 0 items")
        || l.contains("no tests ran")
        || l.contains("0 passed")
        || l.contains("total tests: 0")
        || l.contains("tests:       0 total")
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
        // A live result the result file replaced, and a test's later
        // attempts, are one test (round 17: a failing test counted twice).
        let events = store.events_after(&s.run, 0)?;
        let replaced: std::collections::BTreeSet<String> = events
            .iter()
            .filter_map(|e| match &e.kind {
                prova_core::model::EventKind::TestSuperseded { test, .. } => Some(test.unique()),
                _ => None,
            })
            .collect();
        let mut counted = std::collections::BTreeSet::new();
        let finished: Vec<(String, Outcome)> = events
            .into_iter()
            .filter_map(|e| match e.kind {
                // A test the filter skipped did not run: Vitest `-t` and
                // others report the rest as skipped (round 7).
                prova_core::model::EventKind::TestFinished { test, outcome, .. }
                    if outcome != Outcome::Skipped
                        && !replaced.contains(&test.unique())
                        && counted.insert(test.unique()) =>
                {
                    Some((test.display(), outcome))
                }
                _ => None,
            })
            .collect();
        let tests: Vec<String> = finished.iter().map(|(t, _)| t.clone()).collect();
        // The filter is the framework's own syntax: a Go regex (`^TestAdd$`)
        // is anchored to the test name, not to prova's full name (round 6).
        let matches = |t: &str| filter_matches(&filter, t);
        // The parent of a selected subtest runs too (Go runs `TestX` for
        // `-run TestX/case`): it is not unrelated (round 13, Go).
        let leaf = |n: &str| n.rsplit("::").next().unwrap_or(n).to_owned();
        let parent_of_selected = |t: &str| leaf(&filter).starts_with(&format!("{}/", leaf(t)));
        let unrelated: Vec<&String> = tests
            .iter()
            .filter(|t| !matches(t) && !parent_of_selected(t))
            .collect();
        // The framework's own "nothing matched" beats a level diagnosis: an
        // opaque run with that message means the filter's syntax is wrong
        // for the framework, not the level (round 8: surefire with -q).
        let lines: Vec<&String> = s
            .output_tail
            .iter()
            .chain(s.notes.iter())
            .chain(s.detail.iter())
            .collect();
        let nothing_matched = lines
            .iter()
            .find(|l| {
                let l = l.to_lowercase();
                l.contains("no tests matching")
                    || l.contains("no test matches")
                    || l.contains("no tests ran")
                    || l.contains("no tests found")
                    || l.contains("no tests to run")
                    || l.contains("file or directory not found")
            })
            .map(|l| render::clean(l.trim()));
        let (ok, why) = if let (Some(line), true) =
            (rejected_k_expression(&lines), tests.is_empty())
        {
            (
                false,
                format!(
                    "pytest rejected the filter {filter:?} as a `-k` expression ({line}). `-k` takes a plain substring; select one test or case by its node id (tests/test_x.py::test_name[case]) in a select template with `-k {{filter}}`: prova passes a node id to pytest without the `-k`"
                ),
            )
        } else if let (Some(line), true) = (&nothing_matched, tests.is_empty()) {
            let split = engine::split_filter_fragment(&filter, &lines);
            (
                false,
                format!(
                    "filter {filter:?} matched no test; the framework says: {line}. {}{}",
                    filter_syntax_advice(&s.raw_command),
                    split.map_or(String::new(), |f| format!(
                        " The filter was split into words by a wrapper or the shell ({f:?} is only a part of it): use a filter without spaces (pytest: `-k` with a substring)"
                    )),
                ),
            )
        } else if let (true, Some(cause)) = (
            tests.is_empty()
                && (s.level == prova_core::model::Level::Opaque
                    || matches!(s.exit_code, Some(126 | 127))),
            run_failure_cause(&s),
        ) {
            // The command itself did not work: say why before anything about
            // levels or templates (round 15: exit 127 was "level opaque").
            (
                false,
                format!(
                    "the command failed before any test ran: {cause}; fix it, then verify again"
                ),
            )
        } else if s.level == prova_core::model::Level::Opaque {
            // The framework's own count says nothing ran: the filter is the
            // cause, not the level (round 17: `-Dtest=CalcTest$Division`
            // reached the container expanded, 0 tests, exit 0).
            let zero = lines.iter().any(|l| names_zero_tests(l));
            let blame = if zero {
                format!(
                    "the framework ran 0 tests for filter {filter:?}: the filter did not reach the framework or matched nothing (a wrapper or the shell may have changed it); read `prova output {} --tail 40`",
                    s.run
                )
            } else {
                format!(
                    "cannot verify: no per-test results, so the filter {filter:?} cannot be checked. If the output shows 0 tests, the filter did not reach the framework or matched nothing (a wrapper or the shell may have changed it): `prova output {} --tail 40`. Otherwise the run is at level opaque: see the prova-<language> skill to raise the level",
                    s.run
                )
            };
            (false, blame)
        } else if tests.is_empty() {
            (
                false,
                format!(
                    "no test ran for filter {filter:?}; the select template does not reach the test"
                ),
            )
        } else if !unrelated.is_empty() {
            // A parameterized case cannot be selected alone by Surefire or
            // VSTest: they select by method, so all its cases run (round 17).
            let method = filter
                .split(['(', '['])
                .next()
                .unwrap_or(&filter)
                .trim_end();
            let by_method =
                method.len() < filter.len() && unrelated.iter().all(|t| t.starts_with(method));
            (
                false,
                if by_method {
                    format!(
                        "{} other invocations of {method:?} ran with the selected one: the framework selects by method, so every invocation runs (this is not a template problem)",
                        unrelated.len()
                    )
                } else {
                    format!(
                        "{} tests ran that do not match {filter:?} (e.g. {}); select is not selective",
                        unrelated.len(),
                        unrelated[0]
                    )
                },
            )
        } else {
            let failed: Vec<&str> = finished
                .iter()
                .filter(|(t, o)| matches!(o, Outcome::Failed | Outcome::Errored) && matches(t))
                .map(|(t, _)| t.as_str())
                .collect();
            let mut why = format!("{} matching test(s) ran and nothing else", tests.len());
            if !failed.is_empty() {
                why.push_str(&format!(
                    "; the selected test failed: {} (the filter works, the test does not pass: `prova status {}`)",
                    failed.join(", "),
                    s.run
                ));
            }
            (true, why)
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
    let mut p = prova_adapters::adopt::propose(&root);
    if let Some(wrapper) = earlier_wrapper(&store, &root) {
        prova_adapters::adopt::apply_wrapper_form(&mut p, &wrapper);
    }
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
        // A short nextest slow-timeout (issue #88), only in a project that
        // has no nextest config of its own: an existing file is the owner's.
        let nextest_cfg = root.join(".config/nextest.toml");
        let wrote_nextest = p.nextest_slow_timeout
            && !nextest_cfg.exists()
            && std::fs::create_dir_all(root.join(".config")).is_ok()
            && std::fs::write(&nextest_cfg, prova_adapters::adopt::NEXTEST_SLOW_CONFIG).is_ok();
        if !json {
            if wrote_nextest {
                println!(
                    "wrote .config/nextest.toml (slow-timeout 5s: a hung test is named while it runs)"
                );
            }
            let names: Vec<&str> = p.suites.iter().map(|s| s.name.as_str()).collect();
            println!("wrote prova.toml with suite(s) {}", names.join(", "));
            for n in &p.notes {
                println!("note: {n}");
            }
            // A test prova has already seen, rather than a placeholder
            // (round 15).
            let example: Option<String> = store
                .conn()
                .query_row(
                    "SELECT t.display FROM attempts a JOIN tests t ON t.id = a.test_id
                      WHERE a.outcome = 'passed' AND t.display NOT LIKE '%(%'
                      ORDER BY a.ts_ms DESC LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .ok();
            println!(
                "next: prova adopt --verify {} --filter {}",
                names[0],
                example
                    .map(|e| shell_quote(&e))
                    .unwrap_or_else(|| "<test name>".to_owned())
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
        ExpCmd::Status {
            name,
            comparison,
            json,
        } => {
            let r = experiment::report_at(&store, &name, comparison.as_deref())?;
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
        ExpCmd::Log {
            name,
            entry,
            tail,
            comparison,
        } => {
            let c = match comparison {
                Some(id) => {
                    experiment::check_comparison(&store, &name, &id)?;
                    id
                }
                None => experiment::latest_comparison(&store, &name)?
                    .ok_or_else(|| anyhow!("experiment {name} has no comparison yet"))?,
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
        ExpCmd::Export {
            name,
            comparison,
            output,
        } => {
            let x = prova_core::exp_export::export(&store, &name, comparison.as_deref())?;
            let text = serde_json::to_string_pretty(&x)?;
            match output {
                Some(path) => {
                    std::fs::write(&path, format!("{text}\n"))?;
                    println!("wrote {}", path.display());
                }
                None => println!("{text}"),
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

/// `prova quarantine ...` (ADR 0027).
fn quarantine_cmd(store: &Store, action: QuarantineCmd) -> Result<u8> {
    use prova_core::quarantine as q;
    match action {
        QuarantineCmd::Add {
            test,
            reason,
            days,
            in_file,
        } => {
            let mut found = analysis::find_tests(store, &test, None)?;
            // A name that is exact in a test binary (Rust) is that test,
            // not the same tail of another module's (`flaky_clock` and
            // `tests::flaky_clock`).
            if found.iter().any(|t| {
                t.1 == test
                    && analysis::place_of_test(store, &t.0)
                        .ok()
                        .flatten()
                        .and_then(|p| p.0)
                        .is_some_and(|f| !f.starts_with("(package pending)"))
            }) {
                found.retain(|t| t.1 == test);
            }
            // `--in` names the place in the framework's own form: a Rust
            // test binary, a Go package, a pytest or Vitest file.
            if let Some(w) = in_file.as_deref() {
                let mut places = std::collections::BTreeSet::new();
                let before = found.len();
                let place_of = |t: &(String, String, String)| {
                    analysis::place_of_test(store, &t.0)
                        .ok()
                        .flatten()
                        .unwrap_or_default()
                };
                for t in &found {
                    let (file, path) = place_of(t);
                    places.extend(analysis::place_label(file.as_deref(), &path));
                }
                analysis::retain_in_place(&mut found, w, place_of);
                if found.is_empty() && before > 0 {
                    return Err(UsageError(format!(
                        "{test:?} is not known in {w:?}; it is in: {}",
                        places.into_iter().collect::<Vec<_>>().join(", ")
                    ))
                    .into());
                }
            }
            let mut places_of_names: std::collections::BTreeMap<
                String,
                std::collections::BTreeSet<String>,
            > = Default::default();
            for t in &found {
                let (file, path) = analysis::place_of_test(store, &t.0)?.unwrap_or_default();
                if let Some(p) = analysis::place_label(file.as_deref(), &path) {
                    places_of_names.entry(t.1.clone()).or_default().insert(p);
                }
            }
            let mut names: Vec<String> = found.into_iter().map(|t| t.1).collect();
            names.sort();
            names.dedup();
            // A bare name that is also a qualified test's tail is the same
            // test seen two ways (round 14: gotestsum text, then go json):
            // say which, rather than quarantine the stale one.
            let qualified: Vec<&String> = names
                .iter()
                .filter(|n| **n != test && n.ends_with(&format!("::{test}")))
                .collect();
            let name = if names.contains(&test) && qualified.is_empty() {
                test.clone()
            } else if names.contains(&test) {
                return Err(UsageError(format!(
                    "{test:?} is also known qualified, as {}; quarantine the name your suite reports now (see `prova status last`)",
                    qualified.iter().take(5).map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                ))
                .into());
            } else {
                match names.as_slice() {
                    [one] => one.clone(),
                    [] => {
                        return Err(UsageError(format!(
                            "no test matching {test:?}: prova knows a test once a run showed it individually (`prova history \"\"` lists them)"
                        ))
                        .into());
                    }
                    many => {
                        // Say how to pick one: by its full name, or by the
                        // place it lives in (round 17).
                        let listed = many
                            .iter()
                            .take(5)
                            .map(|n| match places_of_names.get(n) {
                                Some(p) if !p.is_empty() => format!(
                                    "{n} (in {})",
                                    p.iter().cloned().collect::<Vec<_>>().join(", ")
                                ),
                                _ => n.clone(),
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        return Err(UsageError(format!(
                            "{} tests match {test:?}; name one in full, or say where with --in <place>: {listed}",
                            many.len(),
                        ))
                        .into());
                    }
                }
            };
            // Tests of several binaries or modules sharing the name: only
            // the one named with --in is quarantined (round 16: a quarantine
            // of crate calc's `tests::flaky` covered crate geo's failure).
            // A file-level entry is an error, not a test: it cannot be
            // quarantined (round 16).
            if [
                "(file)",
                "(package)",
                "(suite)",
                "(unhandled error)",
                "(unhandled errors)",
            ]
            .iter()
            .any(|p| name.ends_with(p))
            {
                return Err(UsageError(format!(
                    "{name:?} is not a test but a file, package or suite that failed to load or run: a quarantine marks tests only; fix what keeps it from loading"
                ))
                .into());
            }
            let files = q::files_of(store, &name)?;
            let mut matching: Vec<&String> = files.iter().collect();
            if let Some(w) = in_file.as_deref() {
                analysis::retain_in_place(&mut matching, w, |x| (Some((*x).clone()), Vec::new()));
            } else {
                matching.clear();
            }
            let file = match (in_file.as_deref(), files.as_slice()) {
                // A name that is no Rust binary's (a Go package's test): the
                // place already filtered the candidates and the full name
                // says it.
                (Some(_), []) => None,
                (Some(_), _) if matching.len() == 1 => Some(matching[0].clone()),
                (Some(f), _) => {
                    return Err(UsageError(format!(
                        "{name:?} is not known in {f:?}, or in more than one of its places; it is in: {}",
                        files.join(", ")
                    ))
                    .into());
                }
                (None, [one]) => Some(one.clone()),
                (None, []) => None,
                (None, many) => {
                    return Err(UsageError(format!(
                        "{} tests are named {name:?}, in: {}; say which with --in <one of them>",
                        many.len(),
                        many.join(", ")
                    ))
                    .into());
                }
            };
            let e = q::add(store, &name, file.as_deref(), &reason, days, now_ms())?;
            println!(
                "quarantined {} ({}; {}): it still runs and keeps its history, and every run lists it; a run where it fails still fails. Kept in this machine's prova store: CI and teammates do not see it",
                render::clean(&e.test),
                render::clean(&e.reason),
                render::expires_in(e.expires_ms)
            );
            Ok(exit::PASSED)
        }
        QuarantineCmd::List { all, json } => {
            let list = q::list(store, all)?;
            if json {
                println!("{}", serde_json::to_string(&list)?);
            } else if list.is_empty() {
                println!("no quarantined tests");
            } else {
                for e in &list {
                    let state = if e.removed_ms.is_some() {
                        "removed".to_owned()
                    } else {
                        render::expires_in(e.expires_ms)
                    };
                    println!(
                        "{}  ({}; {})",
                        render::named(&e.test, e.file.as_deref()),
                        render::clean(&e.reason),
                        state
                    );
                }
            }
            Ok(exit::PASSED)
        }
        QuarantineCmd::Remove { test, in_file } => {
            let open = q::list(store, false)?;
            let exact: Vec<&q::Entry> = open.iter().filter(|e| e.test == test).collect();
            let mut hits: Vec<&q::Entry> = if exact.is_empty() {
                open.iter().filter(|e| e.test.contains(&test)).collect()
            } else {
                exact
            };
            // A name quarantined in several places: `--in` says which.
            if let Some(w) = in_file.as_deref() {
                // A Go test has no binary: its package heads its name.
                analysis::retain_in_place(&mut hits, w, |e| {
                    let path = match (&e.file, e.test.rsplit_once("::")) {
                        (None, Some((head, leaf))) => vec![head.to_owned(), leaf.to_owned()],
                        _ => Vec::new(),
                    };
                    (e.file.clone(), path)
                });
            }
            let entry = match hits.as_slice() {
                [one] => *one,
                [] => {
                    return Err(UsageError(format!(
                        "no quarantined test matches {test:?}{} (`prova quarantine list` shows them)",
                        in_file
                            .as_deref()
                            .map(|w| format!(" in {w:?}"))
                            .unwrap_or_default()
                    ))
                    .into());
                }
                many => {
                    let listed: Vec<String> = many
                        .iter()
                        .take(5)
                        .map(|e| render::named(&e.test, e.file.as_deref()))
                        .collect();
                    return Err(UsageError(format!(
                        "{} quarantined tests match {test:?}; say which with --in <place>: {}",
                        many.len(),
                        listed.join(", ")
                    ))
                    .into());
                }
            };
            q::remove(store, &entry.test, entry.file.as_deref(), now_ms())?;
            println!(
                "removed the quarantine of {}: it counts again",
                render::named(&entry.test, entry.file.as_deref())
            );
            Ok(exit::PASSED)
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

/// The first wait of a run through a container client, when the caller gave
/// no --max-wait: its stall is called after 60 s (ADR 0031 §4).
const CONTAINER_FIRST_WAIT: Duration = Duration::from_secs(65);

/// The first wait's length when the run's own command is a container
/// client: the caller gave no --max-wait, so the wait is as long as the
/// stall needs, and the announcement says so up front.
fn first_wait_budget(
    until: UntilArg,
    default_budget: bool,
    through_container: bool,
    budget: Duration,
) -> Duration {
    if default_budget
        && through_container
        && matches!(until, UntilArg::FirstFailure)
        && budget < CONTAINER_FIRST_WAIT
    {
        CONTAINER_FIRST_WAIT
    } else {
        budget
    }
}

/// Whether the run's command is itself a container client (`docker run ...`).
fn runs_through_container(store: &Store, id: &str) -> bool {
    store
        .run(id)
        .ok()
        .and_then(|row| serde_json::from_value::<RunSpec>(row.spec).ok())
        .is_some_and(|spec| prova_core::resources::container_client(&spec.command.raw()).is_some())
}

/// How much longer a first wait of `budget` waits: the run goes through a
/// container client (its note says so), the caller gave no --max-wait, and
/// the wait ended running.
fn container_wait_extension(
    default_budget: bool,
    reason: WaitReason,
    notes: &[String],
    budget: Duration,
) -> Option<Duration> {
    (default_budget
        && reason == WaitReason::Running
        && budget < CONTAINER_FIRST_WAIT
        && notes
            .iter()
            .any(|n| n.starts_with("the tests ran through `")))
    .then(|| CONTAINER_FIRST_WAIT - budget)
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
                "no kept output for a failing test matching {name:?} in run {id}: the test printed none before the run ended (pytest and libtest print failure details only in their end-of-run summary, which a cancel or kill cuts off; a test cut off before it finished has none either); `prova output {id}` shows the run's kept output"
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
        // Only kept lines can be shown: a passed run keeps its last lines
        // (ADR 0016). Say so when this asks for everything kept and some
        // lines are not (the plain output marks them "elided").
        if let Ok(Kept::Excerpt(e)) = output::run_excerpt(store, id)
            && n as u64 >= e.kept_lines()
            && e.total_lines > e.kept_lines()
        {
            println!(
                "note: {} line(s) of this run's output were not kept (a passed run keeps only its last lines, a failed one its first lines, error lines and last lines); `--keep-output` on the run keeps all",
                e.total_lines - e.kept_lines()
            );
        }
        for l in engine::output_tail(store, id, n) {
            println!("{l}");
        }
        return Ok(exit::PASSED);
    }
    let spool = store.run_dir(id).join("output.log");
    if !store.run(id)?.status.is_terminal() && spool.exists() {
        let mut f = std::fs::File::open(&spool).with_context(|| format!("{}", spool.display()))?;
        let n = std::io::copy(&mut f, &mut std::io::stdout())?;
        if n == 0 {
            // Silence is not an answer (round 13: nothing, exit 0).
            println!("(run {id} has printed nothing yet)");
        }
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
                return Err(UsageError(format!(
                    "{f} comes after `--`, so it would go to the command, not to prova: put prova's flags before `--` (to pass it to the command on purpose, wrap it: prova run -- sh -c '...')"
                ))
                .into());
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
                let max = first_wait_budget(
                    until,
                    max_wait.is_none(),
                    runs_through_container(&store, &id),
                    max,
                );
                if !json {
                    eprintln!(
                        "run {id} started; waiting up to {} for {}",
                        prova_core::ids::format_duration(max),
                        until_name(until)
                    );
                }
                let s = wait_until(&store, &id, until, 0, max, max_wait.is_none())?;
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
            let max = first_wait_budget(
                until,
                max_wait.is_none(),
                runs_through_container(&store, &id),
                max,
            );
            if !json && max_wait.is_none() && !matches!(until, UntilArg::Done) {
                eprintln!(
                    "waiting up to {} for {}",
                    prova_core::ids::format_duration(max),
                    until_name(until)
                );
            }
            let s = wait_until(&store, &id, until, after, max, max_wait.is_none())?;
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
                    let mut s = engine::summarize(&store, &id, reason, seq)?;
                    // A stalled run is STALLED here as in `wait` (round 13:
                    // status said RUNNING seconds after wait said STALLED).
                    if s.reason == WaitReason::Running && s.stalled_ms.is_some() {
                        s = engine::summarize(&store, &id, WaitReason::Stall, seq)?;
                    }
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
            in_place,
            window,
            json,
        } => {
            let (store, _, _) = open_here()?;
            let mut found = analysis::find_tests(&store, &test, suite.as_deref())?;
            if let Some(w) = in_place.as_deref() {
                let before = found.len();
                let mut places = std::collections::BTreeSet::new();
                let place_of = |t: &(String, String, String)| {
                    analysis::place_of_test(&store, &t.0)
                        .ok()
                        .flatten()
                        .unwrap_or_default()
                };
                for t in &found {
                    let (file, path) = place_of(t);
                    places.extend(analysis::place_label(file.as_deref(), &path));
                }
                analysis::retain_in_place(&mut found, w, place_of);
                if found.is_empty() && before > 0 {
                    return Err(UsageError(format!(
                        "no test matching {test:?} in {w:?}; it is in: {}",
                        places.into_iter().collect::<Vec<_>>().join(", ")
                    ))
                    .into());
                }
            }
            // Exact names win over partial ones (round 10: 16 matches for a
            // name that is exact in every suite, next to an old renamed one).
            // A name that is exact only in its last part (`TestMod` for
            // `ex/mod::TestMod`) wins over its own subtests (`TestMod/a`).
            let all_found = found.clone();
            let found: Vec<_> = {
                let exact: Vec<_> = found.iter().filter(|t| t.1 == test).cloned().collect();
                let leaf: Vec<_> = found
                    .iter()
                    .filter(|t| t.1.rsplit("::").next() == Some(test.as_str()))
                    .cloned()
                    .collect();
                if !exact.is_empty() {
                    exact
                } else if !leaf.is_empty() {
                    leaf
                } else {
                    found
                }
            };
            if found.is_empty() {
                return Err(UsageError(format!(
                    "no test matching {test:?}: prova knows a test once a run showed it individually (`prova history \"\"` lists them)"
                ))
                .into());
            }
            // History is per suite (ADR 0037): one entry per suite that
            // knows the test. Within a suite, one name in several places
            // (two crates' `tests::flaky`) is several tests, each with its
            // place; a bare and a qualified name of one test are one.
            let mut by_suite: std::collections::BTreeMap<&str, Vec<&(String, String, String)>> =
                Default::default();
            for t in &found {
                by_suite.entry(t.2.as_str()).or_default().push(t);
            }
            let mut picks: Vec<(String, String)> = Vec::new();
            let display_of = |id: &str| {
                found
                    .iter()
                    .find(|t| t.0 == id)
                    .map(|t| t.1.clone())
                    .unwrap_or_default()
            };
            let mut ambiguous = false;
            for group in by_suite.values() {
                let group: Vec<(String, String, String)> =
                    group.iter().map(|t| (*t).clone()).collect();
                let mut by_place: std::collections::BTreeMap<
                    String,
                    Vec<(String, String, String)>,
                > = Default::default();
                let mut unknown: Vec<(String, String, String)> = Vec::new();
                for t in &group {
                    match analysis::place_of_test(&store, &t.0)? {
                        Some((Some(file), _)) if !file.starts_with("(package pending)") => {
                            by_place.entry(file).or_default().push(t.clone());
                        }
                        _ => unknown.push(t.clone()),
                    }
                }
                // Tests of unknown place are one more place beside the known
                // ones (a quiet `cargo test` next to a crate's `tests::x`).
                if !by_place.is_empty() && !unknown.is_empty() {
                    by_place.insert("unknown place".to_owned(), unknown);
                }
                let displays: std::collections::BTreeSet<&str> =
                    group.iter().map(|t| t.1.as_str()).collect();
                if by_place.len() > 1 && displays.len() == 1 {
                    for places in by_place.values() {
                        let id = analysis::most_recent(&store, places)?;
                        picks.push((display_of(&id), id));
                    }
                } else if group.len() == 1 || displays.len() == 1 {
                    let id = analysis::most_recent(&store, &group)?;
                    picks.push((display_of(&id), id));
                } else {
                    // Several names, one test: they share one history (round 6).
                    let shared = analysis::shared_ids(&store, &group[0].0)?;
                    if group.iter().all(|f| shared.contains(&f.0)) {
                        let id = analysis::most_recent(&store, &group)?;
                        picks.push((display_of(&id), id));
                    } else {
                        ambiguous = true;
                    }
                }
            }
            // Suites keep their own history: different names are different
            // tests, whichever suite knows them.
            ambiguous |= picks.iter().any(|(d, _)| *d != picks[0].0);
            if ambiguous {
                // One line per name, its suites together: a name listed
                // once per suite read as tests no name could tell apart
                // (round 15).
                let names = analysis::Names::new(&store);
                let mut by_name: std::collections::BTreeMap<String, Vec<&str>> = Default::default();
                for (id, display, suite) in &found {
                    let file = analysis::place_of_test(&store, id)?.and_then(|p| p.0);
                    by_name
                        .entry(names.name(display, file.as_deref()))
                        .or_default()
                        .push(suite);
                }
                // An empty name asks for the list (as the help says).
                if test.is_empty() {
                    println!("{} tests prova knows:", by_name.len());
                } else {
                    println!("{} tests match {test:?}; be more specific:", by_name.len());
                }
                for (display, suites) in by_name.iter().take(20) {
                    println!("  {display}  (suite {})", suites.join(", "));
                }
                if by_name.len() > 20 {
                    println!("  ... and {} more", by_name.len() - 20);
                }
                return Ok(if test.is_empty() {
                    exit::PASSED
                } else {
                    exit::USAGE
                });
            }
            let mut all = Vec::new();
            for (_, pick) in &picks {
                match analysis::stats(&store, pick, window)? {
                    Some(s) => all.push(s),
                    None => bail!("test disappeared"),
                }
            }
            if json {
                // One array, one entry per suite, whatever their number.
                println!("{}", serde_json::to_string(&all)?);
            }
            for (i, s) in all.iter().enumerate().filter(|_| !json) {
                if i > 0 {
                    println!();
                }
                render::test_stats(s);
            }
            // The exact match was shown: say what else the name matches
            // instead of hiding it (round 18: `flaky_clock` showed the
            // integration tests and not `tests::flaky_clock`).
            if !json && all_found.len() > found.len() {
                let rest: std::collections::BTreeSet<&str> = all_found
                    .iter()
                    .filter(|t| !found.iter().any(|f| f.0 == t.0))
                    .map(|t| t.1.as_str())
                    .filter(|d| !found.iter().any(|f| f.1 == *d))
                    .collect();
                if !rest.is_empty() {
                    let shown: Vec<&str> = rest.iter().take(8).copied().collect();
                    println!(
                        "also matches {} other test(s): {}{} (`prova history <name>` for one)",
                        rest.len(),
                        shown.join(", "),
                        if rest.len() > shown.len() {
                            ", ..."
                        } else {
                            ""
                        }
                    );
                }
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
                            render::named(&t.display, t.place.as_deref())
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
                println!(
                    "no flaky tests in recorded history (flaky means different results on the same code; a pass on edited code and a failure on clean code is not that)"
                );
            } else {
                // Columns named, and a quarantine shown (round 16).
                let quarantined = prova_core::quarantine::in_force_at(&store, now_ms())?;
                println!("fail%   retry-passes  code versions flipped  test");
                let labels = render::suite_labels(list.iter().map(|s| {
                    (
                        render::named(&s.display, s.place.as_deref()),
                        s.suite.as_str(),
                    )
                }));
                for (s, label) in list.iter().zip(labels) {
                    let covered = std::iter::once(&s.display).chain(&s.aliases).any(|n| {
                        let key = format!("{}\u{1f}{n}", s.place.as_deref().unwrap_or(""));
                        prova_core::quarantine::covers_key(&quarantined, &key).is_some()
                    });
                    let q = if covered { "  (quarantined)" } else { "" };
                    println!(
                        "{:5.1}%  {:12}  {:21}  {}{q}",
                        s.failure_rate * 100.0,
                        s.passed_on_retry,
                        s.flips_on_same_code,
                        label
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
                if d.out_of_sight > 0 {
                    println!(
                        "unknown: {} of them ran through a container client (docker, podman, ...): what tests use there is out of prova's sight and cannot be observed; declare their size with `size = \"small\"` (or medium, large) under `[suite.<name>]` in prova.toml",
                        d.out_of_sight
                    );
                }
                if d.unknown > d.out_of_sight {
                    println!(
                        "unknown: {} seen small in fewer than {} runs so far; the others were never seen on their own (one test running, or its own process) and their runs were not sampled in time to tell (their output came all at once). Sizes are a lower bound: prova samples every 0.5 s and cannot see sleeps",
                        d.small_so_far,
                        analysis::SMALL_AFTER
                    );
                }
                for (t, label) in d.larger.iter().take(20) {
                    println!("  {}: {}", render::clean(t), render::clean(label));
                }
                // The unknown ones by name: a count cannot be checked.
                let unknown: Vec<&String> = d
                    .tests
                    .iter()
                    .filter(|(_, l)| l == "unknown")
                    .map(|(t, _)| t)
                    .collect();
                if !unknown.is_empty() {
                    let shown: Vec<String> =
                        unknown.iter().take(10).map(|t| render::clean(t)).collect();
                    println!(
                        "unknown: {}{}",
                        shown.join(", "),
                        if unknown.len() > shown.len() {
                            format!(
                                ", and {} more (`--json` lists all)",
                                unknown.len() - shown.len()
                            )
                        } else {
                            String::new()
                        }
                    );
                }
            }
            Ok(exit::PASSED)
        }
        Cmd::Slowest { suite, count, json } => {
            let (store, _, _) = open_here()?;
            let mut rows = analysis::slowest(&store, suite.as_deref(), usize::MAX, 20)?;
            let more = rows.len().saturating_sub(count);
            rows.truncate(count);
            render::slowest(&rows, json);
            if more > 0 && !json {
                println!(
                    "... and {more} more (`prova slowest -n {}` lists all)",
                    count + more
                );
            }
            Ok(exit::PASSED)
        }
        Cmd::Quarantine { action } => {
            let (store, _, _) = open_here()?;
            quarantine_cmd(&store, action)
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
                if let Some(w) = &d.warning {
                    println!("warning: {w}");
                }
                for (label, v) in [
                    ("newly failing", &d.newly_failing),
                    ("fixed", &d.fixed),
                    ("still failing", &d.still_failing),
                    ("changed, but known flaky", &d.flaky),
                    ("added", &d.added),
                    // Not "removed": a reporter may print fewer tests (Vitest
                    // collapses passing files), a suite may be another one.
                    ("not visible in this run", &d.removed),
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
            let mut observer = prova_adapters::observer_for(&spec, &root, Path::new(&row.cwd));
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
    // Tests skipped whenever they appeared.
    let mut skipped_in: std::collections::BTreeMap<String, u32> = Default::default();
    let mut quarantined = std::collections::BTreeSet::new();
    let (mut quiet, mut unverified) = (0u32, Vec::<String>::new());
    let mut unverified_why: Option<String> = None;
    let mut failed_why: Option<String> = None;
    // Runs in which each test had a result.
    let mut seen_in: std::collections::BTreeMap<String, u32> = Default::default();
    for i in 1..=n {
        let left = budget.saturating_sub(start.elapsed());
        if left.is_zero() {
            break;
        }
        let id = engine::create_run(store, request)?;
        let mut child =
            engine::spawn_supervisor(store, &exe, &id).context("starting supervisor")?;
        let s = wait_until(store, &id, UntilArg::Done, 0, left, false)?;
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
        // Quiet output: the framework counted tests prova did not see one
        // by one; that is not "no test ran" (round 15).
        let counted = s.framework.is_some_and(|(t, _)| t > 0);
        if ran == 0 && !counted {
            no_tests += 1;
        } else if s.outcome != Some(Outcome::Passed) && s.failures.is_empty() {
            failed_otherwise += 1;
            failed_why = failed_why.or_else(|| s.detail.clone());
        }
        if counted && (ran as u64) < s.framework.map_or(0, |(t, _)| t) {
            quiet += 1;
        }
        // A pass that proved little (cached results, warnings) is no
        // evidence of a steady test (round 15: "every test passed every
        // time" over replayed go test results).
        // A pass whose only reason is a known flaky test counts when the
        // test was known flaky before this run (the run that made it flaky
        // is not a pass of a known flaky test).
        let flaky_only = s.warnings.is_empty() && !s.flaky_passed.is_empty() && {
            let before = analysis::flaky_before(store, &store.run(&id)?.suite, &id)?;
            !s.flaky_passed.iter().any(|t| before.contains(t))
        };
        if s.outcome == Some(Outcome::Passed)
            && (!s.warnings.is_empty() || s.verified == Some(false))
            && !flaky_only
        {
            // The reason itself, not a generic one (round 16: "e.g. cached
            // results" for a known flaky test's pass).
            unverified_why.get_or_insert_with(|| {
                s.warnings.first().cloned().unwrap_or_else(|| {
                    format!("known flaky test(s) passed: {}", s.flaky_passed.join(", "))
                })
            });
            unverified.push(id.clone());
        }
        // A result re-keyed under another name (a Go test once its package
        // is known) counts under that name only (round 15: tallied twice).
        // Every attempt counts (a retry's failure too).
        let mut results: Vec<(String, String, Outcome, bool)> = Vec::new();
        let mut gone = std::collections::BTreeSet::new();
        for e in store.events_after(&id, 0)? {
            match e.kind {
                prova_core::model::EventKind::TestFinished {
                    test,
                    outcome,
                    passed_in_iteration,
                    ..
                } => {
                    // Two binaries' tests of one name are two tests.
                    let named = analysis::Names::new(store).name(&test.display(), test.place());
                    results.push((test.unique(), named, outcome, passed_in_iteration));
                }
                prova_core::model::EventKind::TestSuperseded { test, .. } => {
                    gone.insert(test.unique());
                }
                _ => {}
            }
        }
        let mut this_run = std::collections::BTreeSet::new();
        for (_, display, outcome, late_pass) in
            results.into_iter().filter(|(u, _, _, _)| !gone.contains(u))
        {
            let first = this_run.insert(display.clone());
            if first {
                *seen_in.entry(display.clone()).or_default() += 1;
            }
            let t = tally.entry(display.clone()).or_default();
            if outcome.is_failure() {
                t.1 += 1;
                // `go test -count=N`: a later iteration passed, so the test
                // both failed and passed in this run (round 18).
                if late_pass {
                    t.0 += 1;
                }
            } else if outcome == Outcome::Passed {
                t.0 += 1;
            } else if outcome == Outcome::Skipped && first {
                *skipped_in.entry(display).or_default() += 1;
            }
        }
        for q in &s.quarantined {
            quarantined.insert(match &q.place {
                Some(p) => format!("{} ({p})", q.test),
                None => q.test.clone(),
            });
        }
        if !json {
            let failing: Vec<String> = s
                .failures
                .iter()
                .map(|f| match &f.place {
                    Some(p) => format!("{} ({p})", f.test),
                    None => f.test.clone(),
                })
                .collect();
            let why = if !failing.is_empty() {
                format!(" ({})", failing.join(", "))
            } else if ran == 0 && !counted {
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
            .map(|(t, (p, f))| {
                serde_json::json!({
                    "test": t,
                    "passed": p,
                    "failed": f,
                    // (round 16: a quarantined test's failures looked like
                    // any other's in --json)
                    "quarantined": quarantined.contains(t),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({ "runs": runs, "repeats": done, "tests": tests })
        );
    } else {
        println!("{done} run(s) of the same code:");
        for (t, (p, f)) in &mixed {
            let q = if quarantined.contains(*t) {
                " (quarantined)"
            } else {
                ""
            };
            println!("  FLAKY {}{q}: failed {f} of {}", render::clean(t), p + f);
        }
        for t in &always {
            let q = if quarantined.contains(*t) {
                " (quarantined)"
            } else {
                ""
            };
            // Failing whenever seen is "every time" only if it was seen in
            // every run (round 15: quiet output hid its passes).
            let seen = seen_in.get(*t).copied().unwrap_or(0);
            if seen < done {
                println!(
                    "  FAIL {}{q}: failed each of the {seen} time(s) it was seen; no result in the other {} run(s) (quiet output shows only failures)",
                    render::clean(t),
                    done - seen
                );
            } else {
                println!("  FAIL {}{q}: failed every time", render::clean(t));
                if !q.is_empty() {
                    println!(
                        "  note: {} failed in every run: it is broken, not flaky; a quarantine only hides it (fix it, then `prova quarantine remove`)",
                        render::clean(t)
                    );
                }
            }
        }
        let steady = tally.values().filter(|(p, f)| *f == 0 && *p > 0).count();
        let skipped_every: Vec<&String> = skipped_in
            .iter()
            .filter(|(t, n)| **n >= done && tally.get(*t).is_some_and(|(p, f)| *p == 0 && *f == 0))
            .map(|(t, _)| t)
            .collect();
        if !skipped_every.is_empty() {
            println!(
                "  {} test(s) skipped in every run, so never verified: {}",
                skipped_every.len(),
                skipped_every
                    .iter()
                    .take(5)
                    .map(|t| render::clean(t))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        if no_tests > 0 {
            println!("  {no_tests} of {done} run(s) ran no test (did the selection match one?)");
        }
        if quiet > 0 {
            println!(
                "  {quiet} of {done} run(s) showed only some tests one by one (quiet output): passes there were not seen"
            );
        }
        if !unverified.is_empty() {
            println!(
                "  {} of {done} pass(es) verified little: {} (e.g. {})",
                unverified.len(),
                unverified.join(", "),
                render::clean(unverified_why.as_deref().unwrap_or("see each run's status"))
            );
        }
        if failed_otherwise > 0 {
            println!(
                "  {failed_otherwise} of {done} run(s) failed with no failing test shown: {}",
                render::clean(failed_why.as_deref().unwrap_or("see each run's status"))
            );
        }
        if tally.is_empty() {
            // Nothing to say about tests.
        } else if mixed.is_empty()
            && always.is_empty()
            && no_tests == 0
            && failed_otherwise == 0
            && skipped_every.is_empty()
            && quiet == 0
            && unverified.is_empty()
        {
            println!("  every test passed every time");
        } else if mixed.is_empty() && always.is_empty() {
            println!("  {steady} test(s) passed whenever they ran");
        } else if steady > 0 {
            println!("  {steady} other test(s) passed every time");
        }
        // A known flaky test that passed every time here proves little
        // (round 9: "every test passed every time" for a 22% flake).
        let names = analysis::Names::new(store);
        for f in analysis::flaky(store, Some(&request.suite), 50)? {
            if let Some((p, 0)) = tally.get(&names.name(&f.display, f.place.as_deref()))
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
    default_budget: bool,
) -> Result<Summary> {
    if matches!(until, UntilArg::FirstFailure) {
        let s = engine::wait(store, id, Until::FirstFailure, after, budget)?;
        // A stall through a container client is called after 60 s: a first
        // wait the caller did not size waits long enough to name it
        // (ADR 0031 §4).
        if let Some(more) = container_wait_extension(default_budget, s.reason, &s.notes, budget) {
            // The run turned out to go through a container client while
            // waiting: say that the wait is longer than announced, and by
            // how much; it ends there.
            eprintln!(
                "the tests run through a container client, whose stall is called after 60 s: waiting {} more, up to {} in all",
                prova_core::ids::format_duration(more),
                prova_core::ids::format_duration(budget + more)
            );
            return Ok(engine::wait(
                store,
                id,
                Until::FirstFailure,
                s.last_seq,
                more,
            )?);
        }
        return Ok(s);
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
                // A run that ends inside one poll shows its failures live
                // like one that fails mid-run (round 17: run 8/8 of a
                // repeat printed none while runs 2, 3 and 5 did).
                render::progress_failures(&s, &mut reported);
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
            // pytest found nothing to run (bad selection, empty collection):
            // not a test failure either.
            (Some(Outcome::Errored), Status::Finished)
                if s.exit_code == Some(4)
                    && s.detail
                        .as_deref()
                        .is_some_and(|d| d.starts_with(engine::PYTEST_NO_TEST)) =>
            {
                exit::INFRA
            }
            // The test runner itself is missing (ADR 0017 section 2).
            (Some(Outcome::Errored), Status::Finished)
                if s.detail
                    .as_deref()
                    .is_some_and(|d| d.starts_with(engine::RUNNER_MISSING)) =>
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

    /// Round 18: pytest's node id did not match prova's dotted name.
    #[test]
    fn a_pytest_node_id_matches_its_prova_name() {
        assert!(filter_matches(
            "tests/test_calc.py::test_add",
            "tests.test_calc::test_add"
        ));
        assert!(!filter_matches(
            "tests/test_calc.py::test_add",
            "tests.test_calc::test_sub"
        ));
        // The line pytest 8.3.4 prints (run in the prova-fixture-python image:
        // `pytest -k "test_a[x y]"`).
        let l = "ERROR: Wrong expression passed to '-k': test_a[x y]: at column 10: expected end of input; got identifier".to_owned();
        assert!(super::rejected_k_expression(&[&l]).is_some());
        assert!(super::rejected_k_expression(&[]).is_none());
    }
}

#[cfg(test)]
mod tests {
    /// Round 18: adopt proposed `./ci.sh` from ad-hoc runs after it was gone.
    #[test]
    fn a_wrapper_is_learned_only_if_its_program_exists() {
        let d = std::env::temp_dir().join(format!("prova-learn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("dev"),
            "#!/bin/sh\nexec docker run --rm img sh -c \"$*\"\n",
        )
        .unwrap();
        let learn = |a: &[&str]| super::learnable_wrapper(&serde_json::json!({ "argv": a }), &d);
        // One exec of a container client: learned.
        assert!(learn(&["./dev", "python3 -m pytest -q"]).is_some());
        // Gone: never proposed.
        assert!(learn(&["./gone.sh", "python3 -m pytest -q"]).is_none());
        // A wrapper in front of words is learned from any command behind it.
        assert!(learn(&["./dev", "make", "test"]).is_some());
    }

    #[test]
    fn usage_exit_code_matches_clap() {
        assert_eq!(super::exit::USAGE, 2);
    }

    #[test]
    fn a_first_wait_through_a_container_client_outlasts_its_stall() {
        // ADR 0031 §4: a stall there is called after 60 s, and the default
        // first wait is 30 s: the hang was never named in it.
        use super::{WaitReason::*, container_wait_extension as ext};
        use std::time::Duration;
        let n = vec!["the tests ran through `docker`: out of sight".to_owned()];
        let thirty = Duration::from_secs(30);
        assert_eq!(
            ext(true, Running, &n, thirty),
            Some(Duration::from_secs(35))
        );
        // Not when the caller sized the wait, not for other runs, not once
        // the wait ended for another reason.
        assert_eq!(ext(false, Running, &n, thirty), None);
        assert_eq!(ext(true, Running, &[], thirty), None);
        assert_eq!(ext(true, Stall, &n, thirty), None);
    }

    #[test]
    fn a_container_first_wait_is_announced_at_its_real_length() {
        // Round 18: "waiting up to 30.0s", then 60 s+: the announced budget
        // is the one waited.
        use super::{UntilArg::*, first_wait_budget as b};
        use std::time::Duration;
        let thirty = Duration::from_secs(30);
        assert_eq!(b(FirstFailure, true, true, thirty), Duration::from_secs(65));
        assert_eq!(b(FirstFailure, false, true, thirty), thirty);
        assert_eq!(b(FirstFailure, true, false, thirty), thirty);
        assert_eq!(b(Done, true, true, thirty), thirty);
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

/// A word safe to paste into a POSIX shell.
fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "_-./:@=".contains(c))
    {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}
