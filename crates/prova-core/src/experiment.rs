//! Experiments (ADR 0007): a frozen protocol, interleaved trials of a
//! baseline and a candidate, a rank-based verdict, and a decision log for
//! improvement loops. `Comparison` mirrors `spec/experiment.qnt`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rusqlite::{OptionalExtension, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::identity::{code_identity, machine_fingerprint};
use crate::ids::{new_run_id, now_ms, sha256_hex};
use crate::process;
use crate::stats;
use crate::store::{Store, StoreError, private_dir, private_file};

#[derive(Debug, thiserror::Error)]
pub enum ExpError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("database: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Refused(String),
}

pub type Result<T> = std::result::Result<T, ExpError>;

fn refused<T>(msg: impl Into<String>) -> Result<T> {
    Err(ExpError::Refused(msg.into()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Better {
    Lower,
    Higher,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Guardrail {
    pub metric: String,
    pub better: Better,
}

/// Declared before any data; frozen at the first trial.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Protocol {
    pub metric: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    pub better: Better,
    /// Smallest relative change that matters, e.g. 0.02.
    pub min_effect: f64,
    pub alpha: f64,
    /// Trials per arm before the first verdict.
    pub trials: u32,
    pub min_trials: u32,
    /// Inconclusive results add trials up to this many per arm.
    pub max_trials: u32,
    pub warmup: u32,
    #[serde(default)]
    pub guardrails: Vec<Guardrail>,
    pub command: Vec<String>,
    pub kill_after_ms: u64,
    /// Shell command run once in each variant's directory before any trial
    /// (e.g. `npm ci`): a baseline worktree has none of the untracked files
    /// (dependencies, build caches) the working tree has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<String>,
    /// The problem's specification (ADR 0015): files frozen by content hash
    /// with the protocol, so the goal cannot move after registration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub specs: Vec<SpecFile>,
    /// Shell command that must succeed in a variant's directory for that
    /// variant to satisfy the specification (model checker, conformance
    /// replay, property tests, proof checker). Runs once per arm, before
    /// any trial; a violating candidate is rejected unmeasured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oracle: Option<String>,
    /// Paths a candidate may change (globs; `*` within a segment, `**`
    /// across). Empty: anything but test, benchmark and build files
    /// (`HARNESS_FILES`), which decide the verdict (round 8).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
    /// `scope` is the whole of what a candidate may change; otherwise it adds
    /// to the default (round 10: allowing go.mod narrowed it to go.mod).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub scope_only: bool,
}

/// A specification file: path relative to the repository root, sha256.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SpecFile {
    pub path: String,
    pub sha256: String,
}

/// Hard floor: below 5 per arm no exact test can reach p < 0.05 with margin.
pub const HARD_MIN_TRIALS: u32 = 5;

impl Protocol {
    pub fn validate(&self) -> Result<()> {
        if self.command.is_empty() {
            return refused("protocol needs a workload command after --");
        }
        if self.min_trials < HARD_MIN_TRIALS {
            return refused(format!(
                "min trials must be at least {HARD_MIN_TRIALS} per arm"
            ));
        }
        if !(self.min_trials <= self.trials && self.trials <= self.max_trials) {
            return refused("need min_trials <= trials <= max_trials");
        }
        if !(self.alpha > 0.0 && self.alpha < 0.5) {
            return refused("alpha must be in (0, 0.5)");
        }
        if !(self.min_effect >= 0.0 && self.min_effect.is_finite()) {
            return refused("min effect must be >= 0");
        }
        Ok(())
    }

    pub fn hash(&self) -> String {
        sha256_hex(serde_json::to_string(self).unwrap_or_default().as_bytes())[..12].to_owned()
    }
}

// ------------------------------------------------------------ lifecycle

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Arm {
    Baseline,
    Candidate,
}

impl Arm {
    pub fn as_str(self) -> &'static str {
        match self {
            Arm::Baseline => "baseline",
            Arm::Candidate => "candidate",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Accept,
    Reject,
    Inconclusive,
    Invalid,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Accept => "accept",
            Verdict::Reject => "reject",
            Verdict::Inconclusive => "inconclusive",
            Verdict::Invalid => "invalid",
        }
    }
}

/// The comparison state machine of `spec/experiment.qnt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparison {
    pub min_trials: u32,
    pub max_trials: u32,
    pub base: u32,
    pub cand: u32,
    pub fingerprints: std::collections::BTreeSet<String>,
    pub verdict: Option<Verdict>,
    /// Specification check per arm (`None` = not checked yet).
    pub base_spec: Option<bool>,
    pub cand_spec: Option<bool>,
}

impl Comparison {
    pub fn new(min_trials: u32, max_trials: u32) -> Comparison {
        Comparison {
            min_trials,
            max_trials,
            base: 0,
            cand: 0,
            fingerprints: Default::default(),
            verdict: None,
            base_spec: None,
            cand_spec: None,
        }
    }

    /// Records whether an arm satisfies the specification; allowed once per
    /// arm, before any trial.
    pub fn check_spec(&mut self, arm: Arm, holds: bool) -> bool {
        let slot = match arm {
            Arm::Baseline => &mut self.base_spec,
            Arm::Candidate => &mut self.cand_spec,
        };
        if self.base + self.cand > 0 || self.verdict.is_some() || slot.is_some() {
            return false;
        }
        *slot = Some(holds);
        true
    }

    /// Rejects, unmeasured, a candidate that violates the specification.
    pub fn reject_violation(&mut self) -> Option<Verdict> {
        if self.verdict.is_some() || self.base_spec != Some(true) || self.cand_spec != Some(false) {
            return None;
        }
        self.verdict = Some(Verdict::Reject);
        self.verdict
    }

    pub fn can_change_protocol(&self) -> bool {
        self.base + self.cand == 0
    }

    /// Next arm to run, interleaved; `None` when no more trials are allowed.
    pub fn next_arm(&self) -> Option<Arm> {
        let open = matches!(self.verdict, None | Some(Verdict::Inconclusive));
        let specified = self.base_spec == Some(true) && self.cand_spec == Some(true);
        if !open || !specified || self.base + self.cand >= 2 * self.max_trials {
            return None;
        }
        // ABBA (spec/experiment.qnt, ADR 0024): even pairs start with the
        // baseline, odd ones with the candidate, so a drift within a pair
        // (boost clocks, warm caches) favours neither arm (round 10: an
        // unchanged candidate was accepted once under load).
        let baseline = if self.base == self.cand {
            self.base.is_multiple_of(2)
        } else {
            self.base < self.cand
        };
        Some(if baseline {
            Arm::Baseline
        } else {
            Arm::Candidate
        })
    }

    pub fn trial(&mut self, fingerprint: &str) -> Option<Arm> {
        let arm = self.next_arm()?;
        self.fingerprints.insert(fingerprint.to_owned());
        match arm {
            Arm::Baseline => self.base += 1,
            Arm::Candidate => self.cand += 1,
        }
        self.verdict = None;
        Some(arm)
    }

    /// Applies a statistical outcome; returns the recorded verdict, or
    /// `None` when deciding is not allowed yet.
    pub fn decide(&mut self, outcome: Verdict) -> Option<Verdict> {
        if self.verdict.is_some()
            || self.base < self.min_trials
            || self.cand < self.min_trials
            || outcome == Verdict::Invalid
        {
            return None;
        }
        let v = if self.fingerprints.len() > 1 {
            Verdict::Invalid
        } else if outcome == Verdict::Inconclusive && self.cand >= self.max_trials {
            Verdict::Reject
        } else {
            outcome
        };
        self.verdict = Some(v);
        Some(v)
    }
}

// ------------------------------------------------------------ verdict

#[derive(Debug, Clone, Serialize)]
pub struct MetricResult {
    pub metric: String,
    pub better: Better,
    pub n_baseline: usize,
    pub n_candidate: usize,
    pub baseline_median: f64,
    pub candidate_median: f64,
    pub baseline_mad: f64,
    pub candidate_mad: f64,
    pub p: f64,
    pub p_adjusted: f64,
    pub method: &'static str,
    /// Hodges-Lehmann shift, candidate minus baseline, in metric units.
    pub shift: f64,
    /// Improvement as a fraction of the baseline median (positive = better).
    pub improvement: f64,
    pub improvement_ci: (f64, f64),
    pub confidence: f64,
    pub outliers_baseline: Vec<usize>,
    pub outliers_candidate: Vec<usize>,
}

pub fn measure(
    metric: &str,
    better: Better,
    base: &[f64],
    cand: &[f64],
    alpha: f64,
    p_adjusted: Option<f64>,
) -> MetricResult {
    let mw = stats::mann_whitney(base, cand);
    let hl = stats::hodges_lehmann(base, cand, alpha);
    let bm = stats::median(base);
    let sign = match better {
        Better::Lower => -1.0,
        Better::Higher => 1.0,
    };
    let scale = if bm != 0.0 { bm.abs() } else { 1.0 };
    let (mut a, mut b) = (sign * hl.lower / scale, sign * hl.upper / scale);
    // A non-negative quantity cannot shrink by more than 100%: an interval
    // reaching past it reads as an error (round 7: "+139.3%").
    if better == Better::Lower && base.iter().chain(cand).all(|v| *v >= 0.0) {
        a = a.min(1.0);
        b = b.min(1.0);
    }
    MetricResult {
        metric: metric.to_owned(),
        better,
        n_baseline: base.len(),
        n_candidate: cand.len(),
        baseline_median: bm,
        candidate_median: stats::median(cand),
        baseline_mad: stats::mad(base),
        candidate_mad: stats::mad(cand),
        p: mw.p,
        p_adjusted: p_adjusted.unwrap_or(mw.p),
        method: mw.method,
        shift: hl.estimate,
        improvement: sign * hl.estimate / scale,
        improvement_ci: (a.min(b), a.max(b)),
        confidence: hl.confidence,
        outliers_baseline: stats::tukey_outliers(base),
        outliers_candidate: stats::tukey_outliers(cand),
    }
}

/// Decision rule (ADR 0007), given the primary result and guardrails.
pub fn judge(
    primary: &MetricResult,
    guardrails: &[MetricResult],
    min_effect: f64,
    alpha: f64,
) -> (Verdict, String) {
    // Near ±100% one decimal hides the rest: 540 ns of 18 ms is +99.997%,
    // not "+100.0%" (round 10).
    let pct = |v: f64| {
        if v.abs() >= 0.995 && v.abs() < 1.0 {
            format!("{:+.3}%", v * 100.0)
        } else {
            format!("{:+.1}%", v * 100.0)
        }
    };
    let (lo, hi) = primary.improvement_ci;
    for g in guardrails {
        let (glo, _) = (-g.improvement_ci.1, -g.improvement_ci.0); // worsening interval
        if g.p < alpha && glo > min_effect {
            return (
                Verdict::Reject,
                format!(
                    "guardrail {} got worse by {} (p={:.3})",
                    g.metric,
                    pct(-g.improvement),
                    g.p
                ),
            );
        }
    }
    // Positive means better in either direction: say so next to the numbers.
    // A shrink of a non-negative quantity stops at 100%; say the bound was
    // cut there rather than print a bare +100.0% (round 8).
    let capped = primary.better == Better::Lower && hi >= 1.0;
    let ci = format!(
        "[{}, {}{}]{}, positive = better",
        pct(lo),
        pct(hi),
        if capped {
            " (upper bound cut at 100%: the interval of differences reaches the whole baseline median, i.e. the candidate is far better, not that it takes no time)"
        } else {
            ""
        },
        ratio_interval(primary.better, lo, hi)
    );
    // The change as a ratio of medians ("37.6x"), easier to read than +97%.
    let (b, c) = (primary.baseline_median, primary.candidate_median);
    let ratio = if b > 0.0 && c > 0.0 && b.is_finite() && c.is_finite() {
        let better = match primary.better {
            Better::Lower => c < b,
            Better::Higher => c > b,
        };
        format!(
            ", median {} -> {} ({:.2}x {})",
            short(b),
            short(c),
            if b > c { b / c } else { c / b },
            if better { "better" } else { "worse" }
        )
    } else {
        String::new()
    };
    // The p in the reason is adjusted when the experiment has several
    // comparisons (Holm): say so (round 6).
    let pname = if (primary.p_adjusted - primary.p).abs() > 1e-12 {
        "adjusted p"
    } else {
        "p"
    };
    // With few trials the exact rank test cannot go lower: say so.
    let floor = crate::stats::min_two_sided_p(primary.n_baseline, primary.n_candidate);
    // Only the raw p can sit at the exact test's floor (an asymptotic p can
    // be lower; round 8 printed "p=0.0075; 0.0079 is the smallest").
    let at_floor = if (primary.p - floor).abs() <= floor * 1e-9 {
        format!(
            "; {:.4} is the smallest p possible with {} + {} trials",
            floor, primary.n_baseline, primary.n_candidate
        )
    } else {
        String::new()
    };
    // A change of three orders of magnitude is rarely what the code does:
    // caching across iterations, a smaller workload, or output the code
    // prints itself (round 8). Accepted, but flagged.
    // A zero median is as implausible (round 9: a cache read as 0 ns).
    let implausible = if (b > 0.0 && c > 0.0 && (b / c).max(c / b) >= 1000.0)
        || (b == 0.0) != (c == 0.0)
    {
        "; warning: a change this large is rarely real: check that the workload still does the same work (no caching across iterations, same input) and that the numbers come from the benchmark, not from the code"
    } else {
        ""
    };
    if primary.p_adjusted < alpha && lo > min_effect {
        (
            Verdict::Accept,
            format!(
                "{} improved {}{ratio} (CI {ci}, {pname}={:.4}{at_floor}){implausible}",
                primary.metric,
                pct(primary.improvement),
                primary.p_adjusted
            ),
        )
    } else if primary.p_adjusted < alpha && hi < 0.0 {
        (
            Verdict::Reject,
            format!(
                "{} got worse by {}{ratio} (CI {ci}, {pname}={:.4}{at_floor})",
                primary.metric,
                pct(-primary.improvement).trim_start_matches('+').to_owned(),
                primary.p_adjusted
            ),
        )
    } else if lo > -min_effect && hi < min_effect {
        (
            Verdict::Reject,
            format!(
                // (round 16: "CI [...], positive = better inside ±5%" read
                // as one garbled phrase)
                "no practical effect, neither better nor worse beyond ±{0}: the CI lies inside ±{0} (CI {ci})",
                pct(min_effect).trim_start_matches('+')
            ),
        )
    } else {
        (
            Verdict::Inconclusive,
            format!(
                "CI {ci} straddles the ±{} threshold ({pname}={:.4})",
                pct(min_effect).trim_start_matches('+'),
                primary.p_adjusted
            ),
        )
    }
}

/// A number with about 4 significant digits, for messages.
/// A number for people: 17588480 reads as 17.59M (round 7).
pub fn short(v: f64) -> String {
    let a = v.abs();
    if a >= 1e9 {
        format!("{:.2}G", v / 1e9)
    } else if a >= 1e6 {
        format!("{:.2}M", v / 1e6)
    } else if a >= 1e4 {
        format!("{:.1}k", v / 1e3)
    } else if a >= 1000.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.4}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_owned()
    }
}

// ------------------------------------------------------------ store

#[derive(Debug, Clone, Serialize)]
pub struct Experiment {
    pub name: String,
    pub hypothesis: String,
    pub protocol: Protocol,
    pub protocol_hash: String,
    pub frozen: bool,
    pub cwd: String,
    pub best: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Variant {
    pub name: String,
    pub kind: String,
    pub commit_sha: Option<String>,
    pub code: String,
    pub params: BTreeMap<String, String>,
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_owned())
}

pub fn get(store: &Store, name: &str) -> Result<Experiment> {
    let row = store
        .conn()
        .query_row(
            "SELECT name, hypothesis, protocol, protocol_hash, frozen, cwd, best FROM experiments WHERE name = ?1",
            params![name],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                ))
            },
        )
        .optional()?;
    let Some((name, hypothesis, protocol, protocol_hash, frozen, cwd, best)) = row else {
        return refused(format!(
            "no experiment named {name:?}; create it with `prova exp new`"
        ));
    };
    Ok(Experiment {
        name,
        hypothesis,
        protocol: serde_json::from_str(&protocol)?,
        protocol_hash,
        frozen: frozen == 1,
        cwd,
        best,
    })
}

pub fn list(store: &Store) -> Result<Vec<Experiment>> {
    let mut stmt = store
        .conn()
        .prepare("SELECT name FROM experiments ORDER BY created_ms DESC")?;
    let names: Vec<String> = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    names.iter().map(|n| get(store, n)).collect()
}

fn valid_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    {
        return refused(format!(
            "invalid name {name:?}: use letters, digits, '-', '_', '.'"
        ));
    }
    Ok(())
}

/// Creates an experiment with its baseline at `baseline_ref` (a git ref,
/// resolved now to a commit) or the working tree when `None`.
pub fn create(
    store: &Store,
    name: &str,
    hypothesis: &str,
    protocol: &Protocol,
    cwd: &Path,
    baseline_ref: Option<&str>,
) -> Result<()> {
    valid_name(name)?;
    protocol.validate()?;
    // The spec is frozen as it is now; the baseline must be that spec, or
    // every comparison fails with a misleading "baseline violates" (round 8).
    if let Some(r) = baseline_ref {
        for f in &protocol.specs {
            let at_ref = Command::new("git")
                .args(["show", &format!("{r}:{}", f.path)])
                .current_dir(store.root())
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| sha256_hex(&o.stdout));
            if at_ref.as_deref() != Some(f.sha256.as_str()) {
                return refused(format!(
                    "specification file {} differs from the baseline {r} (uncommitted changes?): commit it first, so both arms are judged by the same specification",
                    f.path
                ));
            }
        }
    }
    let rel = cwd
        .strip_prefix(store.root())
        .unwrap_or(Path::new(""))
        .to_string_lossy()
        .into_owned();
    let tx = store.conn().unchecked_transaction()?;
    let exists: Option<String> = tx
        .query_row(
            "SELECT name FROM experiments WHERE name = ?1",
            params![name],
            |r| r.get(0),
        )
        .optional()?;
    if exists.is_some() {
        return refused(format!("experiment {name:?} exists; pick another name"));
    }
    tx.execute(
        "INSERT INTO experiments (name, hypothesis, protocol, protocol_hash, cwd, created_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![name, hypothesis, serde_json::to_string(protocol)?, protocol.hash(), rel, now_ms() as i64],
    )?;
    tx.commit()?;
    add_variant(store, name, "baseline", baseline_ref, &BTreeMap::new())
}

pub fn add_variant(
    store: &Store,
    exp: &str,
    name: &str,
    git_ref: Option<&str>,
    params_: &BTreeMap<String, String>,
) -> Result<()> {
    valid_name(name)?;
    if existing_variant(store, exp, name)? {
        return refused(format!("variant {name:?} already exists in {exp}"));
    }
    let root = store.root();
    // A repository without commits has no HEAD to be the baseline: the
    // working tree, snapshotted without a parent, is the baseline.
    let git_ref = match git_ref {
        Some("HEAD")
            if git(root, &["rev-parse", "--git-dir"]).is_some()
                && git(root, &["rev-parse", "--verify", "HEAD^{commit}"]).is_none() =>
        {
            None
        }
        r => r,
    };
    let (kind, sha, code) = match git_ref {
        Some(r) => {
            let Some(sha) = git(root, &["rev-parse", "--verify", &format!("{r}^{{commit}}")])
            else {
                return refused(format!("{r:?} is not a git commit"));
            };
            let tree = git(root, &["rev-parse", &format!("{sha}^{{tree}}")]).unwrap_or_default();
            (
                "ref",
                Some(sha),
                format!("tree:{}", &tree[..tree.len().min(12)]),
            )
        }
        // The working tree is recorded as a snapshot commit, so every
        // comparison runs what was recorded, never the live tree. Outside
        // git there is nothing to snapshot: the variant keeps running in the
        // working tree as before. A snapshot is stored as a `ref` variant
        // (the schema ties a commit to that kind); its `code` keeps the
        // working-tree identity, `+dirty` included.
        None => {
            let sha = snapshot(store, exp, name)?;
            let kind = if sha.is_some() { "ref" } else { "workdir" };
            (kind, sha, code_identity(root))
        }
    };
    let n = store.conn().execute(
        "INSERT OR IGNORE INTO variants (experiment, name, kind, commit_sha, code, params, created_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![exp, name, kind, sha, code, serde_json::to_string(params_)?, now_ms() as i64],
    )?;
    if n == 0 {
        return refused(format!("variant {name:?} already exists in {exp}"));
    }
    Ok(())
}

fn existing_variant(store: &Store, exp: &str, name: &str) -> Result<bool> {
    Ok(store
        .conn()
        .query_row(
            "SELECT 1 FROM variants WHERE experiment = ?1 AND name = ?2",
            params![exp, name],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Records the working tree as a commit without touching the user's branch,
/// HEAD, index or stash: a temporary index holds HEAD plus every non-ignored
/// file (tracked and untracked), `.prova/` excluded. The commit is kept alive
/// by `refs/prova/candidates/<exp>/<label>` so `git gc` cannot drop it. `None`
/// outside a git repository.
fn snapshot(store: &Store, exp: &str, label: &str) -> Result<Option<String>> {
    let Some(top) = git(store.root(), &["rev-parse", "--show-toplevel"]) else {
        return Ok(None);
    };
    let top = PathBuf::from(top);
    let index = store
        .dir()
        .join(format!("snapshot-index-{}", std::process::id()));
    let _ = std::fs::remove_file(&index);
    let run = |args: &[&str]| -> Result<String> {
        let o = Command::new("git")
            .args(args)
            .current_dir(&top)
            .env("GIT_INDEX_FILE", &index)
            .env("GIT_AUTHOR_NAME", "prova")
            .env("GIT_AUTHOR_EMAIL", "prova@localhost")
            .env("GIT_COMMITTER_NAME", "prova")
            .env("GIT_COMMITTER_EMAIL", "prova@localhost")
            .stdin(Stdio::null())
            .output()?;
        if !o.status.success() {
            return refused(format!(
                "could not snapshot the working tree (git {}): {}",
                args.first().copied().unwrap_or(""),
                String::from_utf8_lossy(&o.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_owned())
    };
    let result = (|| {
        let head = git(&top, &["rev-parse", "--verify", "HEAD^{commit}"]);
        if head.is_some() {
            run(&["read-tree", "HEAD"])?;
        }
        let mut add = vec!["add", "-A", "--", ".", ":(exclude,glob)**/.prova/**"];
        // A store kept inside the repository is prova's own, never a candidate.
        // When git already ignores it, naming it makes `git add` refuse
        // (round 7), and it is left out anyway.
        let inside = store
            .dir()
            .strip_prefix(&top)
            .ok()
            .filter(|p| {
                !Command::new("git")
                    .args(["check-ignore", "-q", "--"])
                    .arg(p)
                    .current_dir(&top)
                    .status()
                    .is_ok_and(|s| s.success())
            })
            .map(|p| format!(":(exclude){}", p.display()));
        if let Some(x) = &inside {
            add.push(x);
        }
        run(&add)?;
        let tree = run(&["write-tree"])?;
        let msg = format!("prova candidate {exp}/{label}");
        let mut ct = vec!["commit-tree", &tree, "-m", &msg];
        if let Some(h) = &head {
            ct.extend(["-p", h]);
        }
        let sha = run(&ct)?;
        run(&[
            "update-ref",
            &format!("refs/prova/candidates/{exp}/{label}"),
            &sha,
        ])?;
        Ok(sha)
    })();
    let _ = std::fs::remove_file(&index);
    result.map(Some)
}

pub fn variant(store: &Store, exp: &str, name: &str) -> Result<Variant> {
    let row = store
        .conn()
        .query_row(
            "SELECT name, kind, commit_sha, code, params FROM variants WHERE experiment = ?1 AND name = ?2",
            params![exp, name],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?)),
        )
        .optional()?;
    let Some((name, kind, commit_sha, code, p)) = row else {
        return refused(format!("no variant {name:?} in experiment {exp}"));
    };
    Ok(Variant {
        name,
        kind,
        commit_sha,
        code,
        params: serde_json::from_str(&p)?,
    })
}

pub fn variants(store: &Store, exp: &str) -> Result<Vec<Variant>> {
    let mut stmt = store
        .conn()
        .prepare("SELECT name FROM variants WHERE experiment = ?1 ORDER BY created_ms")?;
    let names: Vec<String> = stmt
        .query_map(params![exp], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    names.iter().map(|n| variant(store, exp, n)).collect()
}

/// Changes the protocol. Refused once the first trial ran.
pub fn set_protocol(store: &Store, name: &str, protocol: &Protocol) -> Result<()> {
    protocol.validate()?;
    let e = get(store, name)?;
    if e.frozen {
        return refused(format!(
            "protocol frozen at first trial (hash {}); start a new experiment: prova exp new {name}-2 ...",
            e.protocol_hash
        ));
    }
    store.conn().execute(
        "UPDATE experiments SET protocol = ?2, protocol_hash = ?3 WHERE name = ?1",
        params![name, serde_json::to_string(protocol)?, protocol.hash()],
    )?;
    Ok(())
}

/// Starts a comparison of `candidate` against `against` (default: the
/// current best). Freezes the protocol. Returns the comparison id.
pub fn start(store: &Store, exp: &str, candidate: &str, against: Option<&str>) -> Result<String> {
    let e = get(store, exp)?;
    let against = against.unwrap_or(&e.best).to_owned();
    if candidate == against {
        return refused(format!("candidate and baseline are both {candidate:?}"));
    }
    variant(store, exp, candidate)?;
    variant(store, exp, &against)?;
    // Only a fair reference: the original baseline or a kept variant that
    // was not reset away from (round 9: a no-op beat a discarded slower
    // variant and became best).
    if !trusted_reference(store, exp, &against)? {
        return refused(format!(
            "{against:?} cannot be the reference: compare against the baseline or a variant that was kept (ACCEPT) and not reset; `prova exp status {exp}` lists them"
        ));
    }
    let running: Option<String> = store
        .conn()
        .query_row(
            "SELECT id FROM comparisons WHERE experiment = ?1 AND status = 'running'",
            params![exp],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = running {
        return refused(format!(
            "comparison {id} is still running; prova exp wait {exp}"
        ));
    }
    let id = new_run_id();
    let tx = store.conn().unchecked_transaction()?;
    tx.execute(
        "INSERT INTO comparisons (id, experiment, baseline, candidate, status, created_ms) VALUES (?1, ?2, ?3, ?4, 'running', ?5)",
        params![id, exp, against, candidate, now_ms() as i64],
    )?;
    tx.commit()?;
    Ok(id)
}

/// Variants of a comparison that run in the live working tree because they
/// were recorded without a snapshot (older stores, or outside git).
pub fn live_tree_variants(store: &Store, exp: &str, names: &[&str]) -> Vec<String> {
    names
        .iter()
        .filter(|n| {
            variant(store, exp, n)
                .map(|v| v.commit_sha.is_none())
                .unwrap_or(false)
        })
        .map(|n| (*n).to_owned())
        .collect()
}

pub fn request_cancel(store: &Store, comparison: &str) -> Result<()> {
    store.conn().execute(
        "UPDATE comparisons SET cancel_requested = 1 WHERE id = ?1",
        params![comparison],
    )?;
    Ok(())
}

// ------------------------------------------------------------ trials

struct TrialResult {
    exit_code: Option<i32>,
    wall_ms: f64,
    metrics: Vec<(String, f64, Option<String>)>,
    /// Lines that reported the primary metric (a forged extra line shows up
    /// as a count the other arm does not have).
    primary_lines: usize,
    /// Processes the workload left running after it ended (`pid comm`).
    leaked: Vec<String>,
}

/// Directory a variant runs in: a detached worktree for ref variants.
fn variant_dir(store: &Store, e: &Experiment, v: &Variant) -> Result<PathBuf> {
    let root = store.root();
    let base = match &v.commit_sha {
        None => root.to_path_buf(),
        Some(sha) => {
            let wt = store.dir().join("worktrees").join(&sha[..12]);
            if !wt.exists() {
                let ok = Command::new("git")
                    .args(["worktree", "add", "--detach", "--force"])
                    .arg(&wt)
                    .arg(sha)
                    .current_dir(root)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()?
                    .success();
                if !ok {
                    return refused(format!("could not create worktree for {sha}"));
                }
            }
            wt
        }
    };
    Ok(if e.cwd.is_empty() {
        base
    } else {
        base.join(&e.cwd)
    })
}

/// Turns a trial's whole output into metrics (name, value, unit); the
/// benchmark recognizers live in prova-adapters.
pub type MetricParser = dyn Fn(&str) -> Vec<(String, f64, Option<String>)>;
/// How many output lines reported each metric (`go test -count 3` reports
/// each benchmark 3 times).
pub type MetricCounter = dyn Fn(&str) -> Vec<(String, usize)>;
/// Reads the tests that passed in a finished command's output (the oracle's).
pub type TestReader = dyn Fn(&str) -> Vec<String>;

fn run_trial(
    dir: &Path,
    p: &Protocol,
    v: &Variant,
    log: &Path,
    parse: &MetricParser,
    count: &MetricCounter,
) -> Result<TrialResult> {
    let mut log = private_file(log)?;
    let mut cmd = arm_command(dir, &p.command[0], &p.command[1..]);
    cmd.current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    for (k, val) in &v.params {
        cmd.env(format!("PROVA_PARAM_{}", k.to_ascii_uppercase()), val);
    }
    let start = Instant::now();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            writeln!(log, "prova: could not start workload: {e}")?;
            return Ok(TrialResult {
                exit_code: None,
                wall_ms: 0.0,
                metrics: vec![],
                primary_lines: 0,
                leaked: vec![],
            });
        }
    };
    let pgid = child.id();
    let err = child.stderr.take();
    let err_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut e) = err {
            let _ = std::io::Read::read_to_end(&mut e, &mut buf);
        }
        buf
    });
    let out = child.stdout.take();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        if let Some(o) = out {
            for line in BufReader::new(o).lines().map_while(std::result::Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        }
    });
    let limit = Duration::from_millis(p.kill_after_ms);
    let mut text = String::new();
    let status = loop {
        while let Ok(line) = rx.try_recv() {
            writeln!(log, "{line}")?;
            text.push_str(&line);
            text.push('\n');
        }
        if let Some(s) = child.try_wait()? {
            break Some(s);
        }
        if start.elapsed() > limit {
            process::kill_group(pgid);
            let _ = child.wait();
            writeln!(log, "prova: killed after {}ms", p.kill_after_ms)?;
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let wall_ms = start.elapsed().as_secs_f64() * 1000.0;
    // Bounded drain of remaining stdout.
    let until = Instant::now() + Duration::from_secs(1);
    while let Ok(line) = rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
        writeln!(log, "{line}")?;
        text.push_str(&line);
        text.push('\n');
    }
    let in_group = process::left_in_group(pgid);
    process::kill_group(pgid); // leftovers must not disturb the next trial
    // A process that left the group (setsid) was reparented to this worker,
    // a child subreaper: it would run on during the other arm's trials and
    // skew them (round 8). Kill it, and report it.
    let mut leaked = in_group;
    for l in process::reap_orphans() {
        // A group member may still be dying when orphans are reaped.
        if !leaked
            .iter()
            .any(|x| x.split(' ').next() == l.split(' ').next())
        {
            leaked.push(l);
        }
    }
    for l in &leaked {
        writeln!(
            log,
            "prova: the workload left a process running after it ended: {l}"
        )?;
    }
    if let Ok(e) = err_thread.join() {
        log.write_all(&e)?;
        text.push_str(&String::from_utf8_lossy(&e));
    }
    let metrics = parse(&text);
    let primary_lines = count(&text)
        .into_iter()
        .find(|(n, _)| *n == p.metric)
        .map_or(0, |(_, c)| c);
    Ok(TrialResult {
        exit_code: status.and_then(|s| s.code()),
        wall_ms,
        metrics,
        primary_lines,
        leaked,
    })
}

/// A worker's scratch root, removed when dropped, on every way out,
/// whatever permissions the workload left.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(e) = remove_tree(&self.0) {
            eprintln!("prova: could not remove {}: {e}", self.0.display());
        }
    }
}

/// Removes a directory tree even when the workload made parts of it
/// read-only (a Go module cache, a read-only unpacked archive).
fn remove_tree(root: &Path) -> std::result::Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fn writable(d: &Path) {
        let Ok(m) = std::fs::symlink_metadata(d) else {
            return;
        };
        if !m.is_dir() {
            return;
        }
        let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700));
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() {
                writable(&e.path());
            }
        }
    }
    writable(root);
    match std::fs::remove_dir_all(root) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

fn fingerprint() -> String {
    sha256_hex(machine_fingerprint().to_string().as_bytes())[..12].to_owned()
}

// ------------------------------------------------------------ worker

/// Body of `prova __exp`: runs trials and decides.
pub fn work(
    store: &Store,
    comparison: &str,
    parse: &MetricParser,
    count: &MetricCounter,
    tests: &TestReader,
) -> Result<()> {
    process::become_subreaper();
    remove_abandoned_scratch();
    // Removed when the comparison ends, however it ends.
    let _scratch = Scratch(scratch_root());
    let mut result = work_comparison(store, comparison, parse, count, tests);
    // An error must end the comparison, or it stays "running" and every
    // wait on it waits for nothing (a refused checkout, round 11).
    if let Err(err) = &result {
        let names: Option<(String, String)> = store
            .conn()
            .query_row(
                "SELECT experiment, candidate FROM comparisons WHERE id = ?1 AND status = 'running'",
                params![comparison],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        if let Some((exp, cand)) = names {
            result = end_failed(store, &exp, comparison, &cand, &err.to_string());
        }
    }
    let cleaned = keep_failed_logs(store, comparison);
    result.and(cleaned)
}

/// The latest complete trial of each arm of a comparison.
fn last_trial_seqs(store: &Store, comparison: &str) -> Result<Vec<u32>> {
    let mut stmt = store.conn().prepare(
        "SELECT MAX(seq) FROM trials WHERE comparison = ?1 AND state = 'complete' GROUP BY arm ORDER BY arm",
    )?;
    let v = stmt
        .query_map(params![comparison], |r| r.get::<_, Option<u32>>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(v.into_iter().flatten().collect())
}

/// After a comparison (ADR 0016): the logs of what failed (a trial, the
/// setup) and the oracle's logs are kept as excerpts in the store; the log directory
/// and the baseline worktrees are removed.
fn keep_failed_logs(store: &Store, comparison: &str) -> Result<()> {
    let logs = store.dir().join("experiments").join(comparison);
    let (exp, base, cand, status): (String, String, String, String) = store.conn().query_row(
        "SELECT experiment, baseline, candidate, status FROM comparisons WHERE id = ?1",
        params![comparison],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let failed_trials: Vec<u32> = {
        let mut stmt = store
            .conn()
            .prepare("SELECT seq FROM trials WHERE comparison = ?1 AND state = 'failed'")?;
        stmt.query_map(params![comparison], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    let mut keep: Vec<String> = failed_trials.iter().map(|s| s.to_string()).collect();
    // A comparison that failed after complete trials (metrics unread or
    // miscounted) keeps each arm's latest trial: the output to read.
    if status == "failed" {
        for seq in last_trial_seqs(store, comparison)? {
            if !keep.contains(&seq.to_string()) {
                keep.push(seq.to_string());
            }
        }
    }
    for arm in ["baseline", "candidate"] {
        if status == "failed" {
            keep.push(format!("setup-{arm}"));
        }
        // The oracle's log is what a "holds" rests on: keep it always
        // (round 7), as an excerpt like any other.
        keep.push(format!("oracle-{arm}"));
    }
    for name in keep {
        if let Some(ex) = crate::output::distill_file(&logs.join(format!("{name}.log")), false)
            && ex.total_lines > 0
        {
            crate::output::keep_comparison(store, comparison, &name, &ex)?;
        }
    }
    let _ = std::fs::remove_dir_all(&logs);
    for name in [base, cand] {
        if let Ok(v) = variant(store, &exp, &name)
            && let Some(sha) = &v.commit_sha
        {
            let wt = store.dir().join("worktrees").join(&sha[..12]);
            if wt.exists() {
                let _ = Command::new("git")
                    .args(["worktree", "remove", "--force"])
                    .arg(&wt)
                    .current_dir(store.root())
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                let _ = std::fs::remove_dir_all(&wt);
            }
        }
    }
    crate::output::enforce_budget(store, crate::output::BUDGET_BYTES)?;
    Ok(())
}

fn work_comparison(
    store: &Store,
    comparison: &str,
    parse: &MetricParser,
    count: &MetricCounter,
    tests: &TestReader,
) -> Result<()> {
    let (exp, base_name, cand_name): (String, String, String) = store.conn().query_row(
        "SELECT experiment, baseline, candidate FROM comparisons WHERE id = ?1",
        params![comparison],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    store.conn().execute(
        "UPDATE comparisons SET worker_pid = ?2 WHERE id = ?1",
        params![comparison, std::process::id()],
    )?;
    let e = get(store, &exp)?;
    let p = &e.protocol;
    let arms = [
        (Arm::Baseline, variant(store, &exp, &base_name)?),
        (Arm::Candidate, variant(store, &exp, &cand_name)?),
    ];
    let dirs = [
        variant_dir(store, &e, &arms[0].1)?,
        variant_dir(store, &e, &arms[1].1)?,
    ];
    let logs = store.dir().join("experiments").join(comparison);
    private_dir(&logs)?;

    let counter = std::cell::Cell::new(0u32);
    // Primary-metric lines per trial, per arm; and the first leaked process.
    let lines: std::cell::RefCell<[Vec<usize>; 2]> = Default::default();
    let leaked: std::cell::RefCell<Option<String>> = Default::default();
    let run_one = |arm: Arm, warmup: bool| -> Result<bool> {
        counter.set(counter.get() + 1);
        let seq = counter.get();
        let i = if arm == Arm::Baseline { 0 } else { 1 };
        let fp = fingerprint();
        let started = now_ms();
        let r = run_trial(
            &dirs[i],
            p,
            &arms[i].1,
            &logs.join(format!("{seq}.log")),
            parse,
            count,
        )?;
        if !warmup {
            lines.borrow_mut()[i].push(r.primary_lines);
        }
        if let Some(l) = r.leaked.first() {
            leaked.borrow_mut().get_or_insert_with(|| {
                format!("{} trial {seq} left process {l} running", arm.as_str())
            });
        }
        let ok = r.exit_code == Some(0) && r.leaked.is_empty();
        let tx = store.conn().unchecked_transaction()?;
        // The protocol freezes at the first trial, not when a comparison
        // starts: one that failed at its oracle measured nothing (round 10).
        tx.execute(
            "UPDATE experiments SET frozen = 1 WHERE name = ?1",
            params![exp],
        )?;
        tx.execute(
            "INSERT INTO trials (comparison, seq, arm, warmup, state, exit_code, fingerprint, started_ms, ended_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![comparison, seq, arm.as_str(), warmup as i64, if ok { "complete" } else { "failed" }, r.exit_code, fp, started as i64, now_ms() as i64],
        )?;
        let mut metrics = r.metrics;
        if !metrics.iter().any(|m| m.0 == "wall_ms") {
            metrics.push(("wall_ms".into(), r.wall_ms, Some("ms".into())));
        }
        for (name, value, unit) in metrics {
            tx.execute(
                "INSERT OR REPLACE INTO samples (comparison, seq, metric, value, unit) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![comparison, seq, name, value, unit],
            )?;
        }
        tx.commit()?;
        Ok(ok)
    };
    let cancelled = || -> Result<bool> {
        Ok(store.conn().query_row(
            "SELECT cancel_requested FROM comparisons WHERE id = ?1",
            params![comparison],
            |r| r.get::<_, i64>(0),
        )? == 1)
    };

    if let Some(setup) = &p.setup {
        for (i, (arm, _)) in arms.iter().enumerate() {
            let log = private_file(&logs.join(format!("setup-{}.log", arm.as_str())))?;
            let ok = arm_command(&dirs[i], "sh", &["-c".to_owned(), setup.clone()])
                .current_dir(&dirs[i])
                .stdin(Stdio::null())
                .stdout(log.try_clone()?)
                .stderr(log)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                let reason = format!(
                    "setup `{setup}` failed for {} (`prova exp log {exp} setup-{}`)",
                    arm.as_str(),
                    arm.as_str()
                );
                return end_failed(store, &exp, comparison, &cand_name, &reason);
            }
        }
    }
    // Tracked files of both checkouts as they are before any oracle or
    // trial: code that rewrites them (its own frozen reference table, the
    // other arm's sources) voids the comparison (round 9).
    let reference = [tracked_state(&dirs[0]), tracked_state(&dirs[1])];
    let tampered = || -> Option<String> {
        (0..2).find_map(|i| {
            let now = tracked_state(&dirs[i]);
            (now != reference[i]).then(|| {
                let first = now
                    .lines()
                    .find(|l| !reference[i].lines().any(|x| x == *l))
                    .or_else(|| reference[i].lines().find(|l| !now.lines().any(|x| x == *l)))
                    .unwrap_or("")
                    .trim()
                    .to_owned();
                format!("{} checkout ({first})", arms[i].0.as_str())
            })
        })
    };
    // The candidate may change only what the protocol allows: test,
    // benchmark and build files decide the verdict (round 8).
    if let (Some(b), Some(cand)) = (&arms[0].1.commit_sha, &arms[1].1.commit_sha) {
        let changed: Vec<String> = git(store.root(), &["diff", "--name-only", b, cand])
            .map(|o| o.lines().map(str::to_owned).collect())
            .unwrap_or_default();
        let outside = out_of_scope(p, &changed);
        if !outside.is_empty() {
            let why = out_of_scope_why(p, &outside);
            let result = serde_json::json!({
                "reason": format!("candidate is out of scope: {why}"),
                "specification": "violated",
            });
            let mut c = Comparison::new(p.min_trials, p.max_trials);
            c.check_spec(Arm::Baseline, true);
            c.check_spec(Arm::Candidate, false);
            c.reject_violation();
            return finish(store, &e, comparison, &cand_name, Verdict::Reject, result);
        }
    }
    let mut c = Comparison::new(p.min_trials, p.max_trials);
    // Tests the baseline's oracle ran: the candidate's must run them too.
    // An oracle exit code alone is no proof: a candidate can add code that
    // skips the checks and still exit 0 (round 7).
    let mut oracle_tests: Vec<String> = Vec::new();
    for (i, (arm, _)) in arms.iter().enumerate() {
        let verdict = satisfies_spec(p, &dirs[i], &logs, *arm)?.and_then(|()| {
            let Some(oracle) = &p.oracle else { return Ok(()) };
            let name = format!("oracle-{}", arm.as_str());
            let out = std::fs::read_to_string(logs.join(format!("{name}.log"))).unwrap_or_default();
            let passed = tests(&out);
            if *arm == Arm::Baseline {
                oracle_tests = passed;
                return Ok(());
            }
            let missing: Vec<&String> = oracle_tests.iter().filter(|t| !passed.contains(t)).collect();
            match missing.first() {
                None => Ok(()),
                Some(first) => Err(SpecFail::Violated(format!(
                    "oracle `{oracle}` exited 0, but {} test(s) that passed in the baseline's oracle did not pass in the candidate's (e.g. {first}): its checks failed or were skipped (`prova exp log <experiment> {name}`)",
                    missing.len()
                ))),
            }
        });
        c.check_spec(*arm, verdict.is_ok());
        if let Err(fail) = verdict {
            let why = match fail {
                SpecFail::Infra(why) => {
                    // Nothing was measured or judged: not a verdict on
                    // either arm.
                    let reason = why.replace("<experiment>", &exp);
                    return end_failed(store, &exp, comparison, &cand_name, &reason);
                }
                SpecFail::Violated(why) => why.replace("<experiment>", &exp),
            };
            if *arm == Arm::Baseline {
                let reason = format!(
                    "baseline violates the specification: {why}; measure only implementations that satisfy it"
                );
                return end_failed(store, &exp, comparison, &cand_name, &reason);
            }
            c.reject_violation();
            let result = serde_json::json!({
                "reason": format!("candidate violates the specification: {why}"),
                "specification": "violated",
            });
            return finish(store, &e, comparison, &cand_name, Verdict::Reject, result);
        }
    }
    if let Some(where_) = tampered() {
        c.reject_violation();
        let result = serde_json::json!({
            "reason": format!("candidate violates the specification: tracked files changed while the oracle ran, in the {where_}: code that rewrites the checkout (its spec, its reference data, the other arm) cannot be judged"),
            "specification": "violated",
        });
        return finish(store, &e, comparison, &cand_name, Verdict::Reject, result);
    }
    for _ in 0..p.warmup {
        for arm in [Arm::Baseline, Arm::Candidate] {
            if !run_one(arm, true)? {
                let left = leaked.borrow().clone();
                let why = left.clone().map_or_else(
                    || format!("{} warmup trial failed", arm.as_str()),
                    |l| format!("{l} after it ended; it would disturb the other arm's trials (`prova exp log {exp}`)"),
                );
                // Leftover processes void the comparison (a revalidation of
                // the accept does not hold); a plain failure does not.
                if left.is_some() {
                    return end_void(store, &exp, comparison, &cand_name, &why);
                }
                return end_failed(store, &exp, comparison, &cand_name, &why);
            }
        }
    }
    let mut target = p.trials;
    loop {
        while c.base < target || c.cand < target {
            if cancelled()? {
                return end_status(store, comparison, "cancelled");
            }
            let Some(arm) = c.next_arm() else { break };
            if !run_one(arm, false)? {
                let left = leaked.borrow().clone();
                let reason = left.clone().map_or_else(
                    || {
                        format!(
                            "{} trial {} failed (exit != 0; `prova exp log {exp} {}`)",
                            arm.as_str(),
                            counter.get(),
                            counter.get()
                        )
                    },
                    |l| {
                        format!(
                            "{l} after it ended; it would disturb the other arm's trials (`prova exp log {exp} {}`)",
                            counter.get()
                        )
                    },
                );
                if left.is_some() {
                    return end_void(store, &exp, comparison, &cand_name, &reason);
                }
                return end_failed(store, &exp, comparison, &cand_name, &reason);
            }
            if let Some(where_) = tampered() {
                let reason = format!(
                    "trial {} changed tracked files in the {where_}: a workload must not rewrite code or data; the comparison is void",
                    counter.get()
                );
                return end_void(store, &exp, comparison, &cand_name, &reason);
            }
            c.trial(&fingerprint_of(store, comparison, counter.get())?);
        }
        // Each arm's workload must report the metric as often per trial as
        // the other: extra lines are forged output (a candidate's code can
        // print benchmark lines), missing ones a changed workload (round 8).
        {
            let l = lines.borrow();
            let usual = |v: &Vec<usize>| v.iter().copied().max().unwrap_or(0);
            let (b, c) = (usual(&l[0]), usual(&l[1]));
            if b != c || l[1].iter().any(|n| *n != c) {
                let seqs = last_trial_seqs(store, comparison)?
                    .iter()
                    .map(|s| format!("`prova exp log {exp} {s}`"))
                    .collect::<Vec<_>>()
                    .join(" or ");
                let see = if seqs.is_empty() {
                    String::new()
                } else {
                    format!("; the last trial of each arm is kept: {seqs}")
                };
                let reason = format!(
                    "the candidate's workload reported {} {}x per trial, the baseline's {b}x: extra lines are forged output, missing ones a changed workload (or a unit the reader does not know); the comparison is void{see}",
                    p.metric, c
                );
                return end_void(store, &exp, comparison, &cand_name, &reason);
            }
        }
        let (outcome, result) = evaluate(store, &e, comparison)?;
        if outcome == Verdict::Inconclusive && target < p.max_trials {
            target = (target + 2).min(p.max_trials);
            c.verdict = Some(Verdict::Inconclusive);
            continue;
        }
        let verdict = c.decide(outcome).unwrap_or(Verdict::Invalid);
        let mut result = result;
        // A weak protocol, or many experiments on the same baseline and
        // metric, make a lucky ACCEPT likelier: say so (round 11).
        let mut caveats = Vec::new();
        // Speed is judged, not correctness: without an oracle or spec files
        // a candidate that does less (or nothing) looks fastest (round 14).
        if p.oracle.is_none() && p.specs.is_empty() {
            caveats.push(
                "no oracle or spec in the protocol: correctness was not checked, only speed"
                    .to_owned(),
            );
        }
        if p.alpha > 0.1 {
            caveats.push(format!("alpha {} is weak (0.05 is usual)", p.alpha));
        }
        if p.min_effect < 0.01 {
            caveats.push(format!(
                "a minimum effect of {:.1}% lets noise count as an improvement",
                p.min_effect * 100.0
            ));
        }
        // Say what could not be checked (scope: say when prova cannot know).
        if cfg!(not(target_os = "linux")) {
            caveats.push(
                "this OS gives prova no way to find processes that left a trial's process group: they were not checked"
                    .to_owned(),
            );
        }
        if let Some(sha) = &arms[0].1.commit_sha {
            let others: i64 = store.conn().query_row(
                "SELECT COUNT(DISTINCT x.name) FROM experiments x JOIN variants v ON v.experiment = x.name
                  WHERE v.name = 'baseline' AND v.commit_sha = ?1 AND x.name <> ?2
                    AND json_extract(x.protocol, '$.metric') = ?3
                    AND EXISTS (SELECT 1 FROM samples s JOIN comparisons k ON k.id = s.comparison
                                 WHERE k.experiment = x.name)",
                params![sha, exp, p.metric],
                |r| r.get(0),
            )?;
            if others > 0 {
                caveats.push(format!(
                    "{others} other experiment(s) measured {} on this baseline: across them one lucky ACCEPT is likelier",
                    p.metric
                ));
            }
        }
        if !caveats.is_empty() {
            result["caveats"] = serde_json::json!(caveats);
        }
        // Say that the specification was checked, not only when it failed
        // (ADR 0018): both arms satisfied it before any trial ran.
        if let Some(oracle) = &p.oracle {
            result["specification"] = serde_json::json!(if oracle_tests.is_empty() {
                format!(
                    "holds on both arms by exit code only (oracle `{oracle}` showed no individual test; make it verbose so skipped checks are caught)"
                )
            } else {
                format!(
                    "holds on both arms (oracle `{oracle}`: the same {} test(s) passed on each)",
                    oracle_tests.len()
                )
            });
        } else if !p.specs.is_empty() {
            result["specification"] =
                serde_json::json!("specification files unchanged on both arms (no oracle)");
        }
        return finish(store, &e, comparison, &cand_name, verdict, result);
    }
}

/// Files that decide a verdict rather than implement the change: tests,
/// benchmarks, build and runner configuration. Without `--change`, a
/// candidate may not touch them (round 8: forged oracle output from an added
/// `TestMain`, a smaller benchmark input, fake benchmark lines).
pub const HARNESS_FILES: &[&str] = &[
    "**/*_test.go",
    "**/test_*.py",
    "**/*_test.py",
    "**/conftest.py",
    "**/*.test.*",
    "**/*.spec.*",
    "**/*.bench.*",
    "**/*Test.java",
    "**/*Tests.java",
    "**/*Test.cs",
    "**/*Tests.cs",
    "**/*Test.kt",
    "**/test/**",
    "**/tests/**",
    "**/__tests__/**",
    "**/bench/**",
    "**/benches/**",
    "**/benchmarks/**",
    "**/spec/**",
    // Expected values and inputs an oracle reads, and build scripts that run
    // inside it (ADR 0032: a candidate must not move what it is judged by)
    "**/testdata/**",
    "**/fixtures/**",
    "**/__snapshots__/**",
    "**/golden/**",
    "**/*.golden",
    "**/*.snap",
    "**/build.rs",
    "**/Makefile",
    "**/GNUmakefile",
    "**/*.mk",
    "prova.toml",
    "**/package.json",
    "**/pytest.ini",
    "**/.pytest.ini",
    "**/pyproject.toml",
    "**/setup.cfg",
    "**/tox.ini",
    "**/Cargo.toml",
    "**/go.mod",
    "**/pom.xml",
    "**/build.gradle*",
    "**/*.csproj",
    "**/*.sln",
    "**/vitest.config.*",
    "**/jest.config.*",
    "**/*.sh",
];

/// A file that decides the verdict rather than implements the change.
fn harness_file(path: &str) -> bool {
    HARNESS_FILES.iter().any(|g| glob_match(g, path))
}

/// Matches a path against a glob: `*` within one segment, `**` any number
/// of segments.
pub fn glob_match(glob: &str, path: &str) -> bool {
    fn seg(p: &str, s: &str) -> bool {
        match p.split_once('*') {
            None => p == s,
            Some((pre, rest)) => {
                s.starts_with(pre)
                    && (0..=s.len() - pre.len()).any(|i| {
                        s.is_char_boundary(pre.len() + i) && seg(rest, &s[pre.len() + i..])
                    })
            }
        }
    }
    fn go(g: &[&str], p: &[&str]) -> bool {
        match g.first() {
            None => p.is_empty(),
            Some(&"**") => (0..=p.len()).any(|i| go(&g[1..], &p[i..])),
            Some(x) => !p.is_empty() && seg(x, p[0]) && go(&g[1..], &p[1..]),
        }
    }
    let g: Vec<&str> = glob.split('/').collect();
    let p: Vec<&str> = path.split('/').collect();
    go(&g, &p)
}

/// Why a candidate that changed `outside` is rejected unmeasured. A file the
/// protocol froze with `--spec` is never offered for `--change`: the
/// specification cannot move (ADR 0035 section 4).
fn out_of_scope_why(p: &Protocol, outside: &[String]) -> String {
    let first = outside.first().map_or("", String::as_str);
    if p.scope_only {
        return format!(
            "candidate changes {} file(s) outside the protocol's --change-only scope, e.g. {first}",
            outside.len()
        );
    }
    let frozen: Vec<&str> = outside
        .iter()
        .filter(|f| p.specs.iter().any(|s| s.path == **f))
        .map(String::as_str)
        .collect();
    let head = format!(
        "candidate changes {} file(s) that decide the verdict (tests, benchmarks, build and runner files), e.g. {first}",
        outside.len()
    );
    if frozen.is_empty() {
        format!(
            "{head}; if the change needs them, allow them with `--change <path>` (`prova exp protocol` before the first trial, or a new experiment)"
        )
    } else {
        format!(
            "{head}; {} is frozen as the specification and cannot be allowed: the candidate must satisfy it, not change it",
            frozen.join(", ")
        )
    }
}

/// Files the candidate changed that the protocol does not let it change.
fn out_of_scope(p: &Protocol, changed: &[String]) -> Vec<String> {
    changed
        .iter()
        .filter(|f| {
            let allowed = p.scope.iter().any(|g| glob_match(g, f));
            if p.scope_only {
                !allowed
            } else {
                !allowed && harness_file(f)
            }
        })
        .cloned()
        .collect()
}

/// A command for one arm with its own caches: nothing one arm compiles is
/// read by the other. The workload must not reach prova's own store (round
/// 9) or Python's user site.
fn arm_command(dir: &Path, program: &str, args: &[String]) -> Command {
    let state = arm_scratch(dir);
    let _ = std::fs::create_dir_all(&state);
    let mut c = Command::new(program);
    c.args(args)
        .env_remove("PROVA_HOME")
        .env("PYTHONNOUSERSITE", "1")
        .env("GOCACHE", state.join("go-build"))
        .env("XDG_CACHE_HOME", state.join("cache"))
        .env("npm_config_cache", state.join("npm"));
    c
}

/// One checkout's scratch space in this worker: its caches (Go, npm, XDG).
/// It lives under the worker's scratch root, which is removed when the
/// comparison ends.
fn arm_scratch(dir: &Path) -> PathBuf {
    let key = sha256_hex(dir.to_string_lossy().as_bytes());
    scratch_root().join(format!("arm-{}", &key[..16]))
}

/// This worker's scratch root, in the temporary directory. It holds a lock
/// file, locked for the worker's whole life, so that no other worker's
/// sweep takes it for an abandoned one. A pid cannot tell: a `/tmp` shared
/// by containers has a pid namespace per container. The root is made under
/// a hidden name and renamed once locked, so a sweep never sees it unlocked.
fn scratch_root() -> PathBuf {
    static ROOT: std::sync::OnceLock<(PathBuf, Option<std::fs::File>)> = std::sync::OnceLock::new();
    let (root, _lock) = ROOT.get_or_init(|| {
        let tmp = std::env::temp_dir();
        let base = std::path::absolute(&tmp).unwrap_or(tmp);
        let id = new_run_id();
        let root = base.join(format!("prova-scratch-{id}"));
        let staging = base.join(format!(".prova-scratch-{id}"));
        let claim = || -> std::io::Result<std::fs::File> {
            private_dir(&staging)?;
            let lock = std::fs::File::create(staging.join(SCRATCH_LOCK))?;
            lock.lock()?;
            std::fs::rename(&staging, &root)?;
            Ok(lock)
        };
        let lock = claim().ok();
        let _ = remove_tree(&staging);
        (root, lock)
    });
    root.clone()
}

const SCRATCH_LOCK: &str = ".lock";

/// Removes the scratch roots of workers that are gone: one killed outright
/// could not remove its own. A root is gone when its lock is free.
fn remove_abandoned_scratch() {
    let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for e in rd.flatten() {
        if !e
            .file_name()
            .to_string_lossy()
            .starts_with("prova-scratch-")
        {
            continue;
        }
        let free =
            std::fs::File::open(e.path().join(SCRATCH_LOCK)).is_ok_and(|f| f.try_lock().is_ok());
        if free {
            let _ = remove_tree(&e.path());
        }
    }
}

/// `git status` of tracked files in a checkout (empty outside git).
fn tracked_state(dir: &Path) -> String {
    Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Whether a variant satisfies the protocol's specification: its spec files
/// are the registered ones, and the oracle succeeds in its directory.
fn satisfies_spec(
    p: &Protocol,
    dir: &Path,
    logs: &Path,
    arm: Arm,
) -> Result<std::result::Result<(), SpecFail>> {
    for f in &p.specs {
        let found = std::fs::read(dir.join(&f.path)).map(|b| sha256_hex(&b));
        match found {
            Ok(h) if h == f.sha256 => {}
            Ok(_) => {
                return Ok(Err(SpecFail::Violated(format!(
                    "{} differs from the registered specification",
                    f.path
                ))));
            }
            Err(_) => {
                return Ok(Err(SpecFail::Violated(format!(
                    "{} is missing (commit the specification before registering it)",
                    f.path
                ))));
            }
        }
    }
    let Some(oracle) = &p.oracle else {
        return Ok(Ok(()));
    };
    let name = format!("oracle-{}.log", arm.as_str());
    let log = private_file(&logs.join(&name))?;
    let status = arm_command(dir, "sh", &["-c".to_owned(), oracle.clone()])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .status()
        .ok();
    let ok = status.is_some_and(|s| s.success());
    // A process the oracle leaves behind would run into the trials and be
    // blamed on them (round 10: a daemon from the candidate's oracle was
    // reported as the baseline trial's leak).
    let left = process::reap_orphans();
    if let Some(l) = left.first() {
        // Not a verdict on the code: a process outliving its oracle is the
        // environment (round 15).
        return Ok(Err(SpecFail::Infra(format!(
            "the oracle left process {l} running after it ended (an infrastructure problem, not a verdict on the code); it would disturb the trials: stop what starts it, or run the oracle in the foreground"
        ))));
    }
    if ok {
        return Ok(Ok(()));
    }
    let arm_log = name.trim_end_matches(".log");
    // 126 and 127 are the shell saying it could not execute the oracle (not
    // executable, command not found): nothing was checked, so the code is
    // not to blame (usability round: `lake: command not found` was reported
    // as the baseline violating the specification).
    if let Some(code @ (126 | 127)) = status.and_then(|s| s.code()) {
        let what = if code == 127 {
            "command not found"
        } else {
            "not executable"
        };
        return Ok(Err(SpecFail::Infra(format!(
            "the oracle could not run (exit {code}: {what}; see `prova exp log <experiment> {arm_log}`): install what it needs"
        ))));
    }
    // The machine ran out of space or quota while the oracle built or ran:
    // nothing was checked (round 16: `Disk quota exceeded` while compiling
    // dependencies was "candidate violates the specification").
    {
        let text = std::fs::read_to_string(logs.join(&name)).unwrap_or_default();
        const NO_ROOM: &[&str] = &[
            "Disk quota exceeded",
            "No space left on device",
            "os error 28",
            "os error 122",
            "Cannot allocate memory",
            "out of memory",
        ];
        if let Some(l) = text.lines().find(|l| NO_ROOM.iter().any(|p| l.contains(p))) {
            return Ok(Err(SpecFail::Infra(format!(
                "the oracle could not run: the machine ran out of room ({}; see `prova exp log <experiment> {arm_log}`): free space and run again",
                l.trim().chars().take(200).collect::<String>()
            ))));
        }
    }
    // The decisive line, so the reader need not open the log.
    let text = std::fs::read_to_string(logs.join(&name)).unwrap_or_default();
    let last = failing_test_lines(&text)
        .or_else(|| decisive_line(&text).map(str::to_owned))
        .map(|l| l.chars().take(300).collect::<String>())
        .unwrap_or_else(|| format!("oracle `{oracle}` failed, printing nothing"));
    Ok(Err(SpecFail::Violated(format!(
        "{last} (full log: prova exp log <experiment> {arm_log})"
    ))))
}

/// A line that only says "something failed" (`go run`'s `exit status 1`,
/// a test runner's `FAIL`, make's and CI's trailers), never what.
fn is_generic_trailer(l: &str) -> bool {
    let low = l.to_ascii_lowercase();
    low.starts_with("exit status")
        || low == "fail"
        || low.starts_with("fail\t")
        || low.starts_with("make: ***")
        || low.starts_with("make[")
        || low.starts_with("error: process completed")
        || low.starts_with("error: process exited")
        || low.starts_with("error: command failed")
        // cargo's own trailers after a failing test binary
        || low.starts_with("error: test failed, to rerun")
        || low.starts_with("error: could not compile")
        || low.starts_with("failures:")
        || low.starts_with("test result: failed")
}

/// The most specific failure in an oracle's output: the failing test with
/// its assertion (`---- t stdout ----` and its `panicked at` lines, or a
/// `test t ... FAILED` line), when the output has one.
fn failing_test_lines(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    if let Some(i) = lines
        .iter()
        .position(|l| l.starts_with("---- ") && l.ends_with(" ----"))
    {
        let body: Vec<&str> = lines[i + 1..]
            .iter()
            .copied()
            .take_while(|l| !(l.starts_with("---- ") || l.starts_with("failures:")))
            .filter(|l| !l.is_empty() && !l.starts_with("note: run with"))
            .take(4)
            .collect();
        return Some(format!(
            "{}: {}",
            lines[i].trim_matches(|c| c == '-' || c == ' '),
            body.join(" ")
        ));
    }
    lines
        .iter()
        .find(|l| l.starts_with("test ") && l.ends_with("FAILED"))
        .map(|l| (*l).to_owned())
}

/// The most informative line of a failed oracle's output: the last one that
/// is not a generic trailer (usability round 13: `exit status 1` hid
/// `CountPrimes(2) = 1, specification says 0`). Falls back to the last line.
fn decisive_line(text: &str) -> Option<&str> {
    let lines = || text.lines().map(str::trim).filter(|l| !l.is_empty());
    lines()
        .rfind(|l| !is_generic_trailer(l))
        .or_else(|| lines().next_back())
}

/// Why a variant does not satisfy the specification: it does not, or the
/// oracle could not run (which says nothing about the code).
enum SpecFail {
    Violated(String),
    Infra(String),
}

/// A large improvement as a ratio interval ("12.0x to 14.3x faster"): a
/// percentage squeezed against 100% hides how big the change is.
fn ratio_interval(better: Better, lo: f64, hi: f64) -> String {
    let (rlo, rhi) = match better {
        Better::Lower if lo >= 0.5 => (
            1.0 / (1.0 - lo),
            if hi < 1.0 {
                Some(1.0 / (1.0 - hi))
            } else {
                None
            },
        ),
        Better::Higher if lo >= 1.0 => (1.0 + lo, Some(1.0 + hi)),
        _ => return String::new(),
    };
    match rhi {
        Some(h) => format!(" = {rlo:.1}x to {h:.1}x better"),
        None => format!(" = at least {rlo:.1}x better"),
    }
}

fn fingerprint_of(store: &Store, comparison: &str, seq: u32) -> Result<String> {
    Ok(store.conn().query_row(
        "SELECT fingerprint FROM trials WHERE comparison = ?1 AND seq = ?2",
        params![comparison, seq],
        |r| r.get(0),
    )?)
}

fn samples(store: &Store, comparison: &str, metric: &str) -> Result<(Vec<f64>, Vec<f64>)> {
    let mut stmt = store.conn().prepare(
        "SELECT t.arm, s.value FROM samples s JOIN trials t ON t.comparison = s.comparison AND t.seq = s.seq
         WHERE s.comparison = ?1 AND s.metric = ?2 AND t.warmup = 0 AND t.state = 'complete' ORDER BY t.seq",
    )?;
    let mut base = Vec::new();
    let mut cand = Vec::new();
    for row in stmt.query_map(params![comparison, metric], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
    })? {
        let (arm, v) = row?;
        if arm == "baseline" {
            base.push(v)
        } else {
            cand.push(v)
        }
    }
    Ok((base, cand))
}

/// Statistical outcome and details for the comparison as it stands.
pub fn evaluate(
    store: &Store,
    e: &Experiment,
    comparison: &str,
) -> Result<(Verdict, serde_json::Value)> {
    let p = &e.protocol;
    let (base, cand) = samples(store, comparison, &p.metric)?;
    if base.len() < p.min_trials as usize || cand.len() < p.min_trials as usize {
        return refused(format!(
            "verdict refused: metric {:?} has {} baseline and {} candidate samples, {} required each (does the workload print it?)",
            p.metric,
            base.len(),
            cand.len(),
            p.min_trials
        ));
    }
    // Holm across this experiment's hypotheses (ADR 0032): a revalidation
    // replicates a hypothesis already in the family, so its p takes the
    // place of the p it replicates and adds no comparison.
    let replicates = replicated(store, &e.name, comparison, i64::MAX)?;
    let mut ps = family(store, &e.name, comparison, i64::MAX, replicates.as_deref())?;
    let raw = stats::mann_whitney(&base, &cand).p;
    ps.push(raw);
    let adjusted = *stats::holm(&ps).last().unwrap_or(&raw);
    let primary = measure(&p.metric, p.better, &base, &cand, p.alpha, Some(adjusted));
    let mut guards = Vec::new();
    for g in &p.guardrails {
        let (gb, gc) = samples(store, comparison, &g.metric)?;
        if gb.len() >= 2 && gc.len() >= 2 {
            guards.push(measure(&g.metric, g.better, &gb, &gc, p.alpha, None));
        }
    }
    let (verdict, reason) = judge(&primary, &guards, p.min_effect, p.alpha);
    let fps: std::collections::BTreeSet<String> = {
        let mut s = store.conn().prepare(
            "SELECT DISTINCT fingerprint FROM trials WHERE comparison = ?1 AND warmup = 0",
        )?;
        s.query_map(params![comparison], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    let reason = if fps.len() > 1 {
        format!(
            "trials ran on {} different machine fingerprints; not comparable",
            fps.len()
        )
    } else {
        reason
    };
    let mut result = serde_json::json!({ "primary": primary, "guardrails": guards, "reason": reason, "comparisons_in_family": ps.len() });
    if let Some(id) = &replicates {
        result["revalidates"] = serde_json::json!(id);
    }
    Ok((verdict, result))
}

/// The earlier ACCEPT a comparison replicates, when it is a revalidation:
/// the kept candidate again against the original baseline (ADR 0032).
fn replicated(
    store: &Store,
    exp: &str,
    comparison: &str,
    before_ms: i64,
) -> Result<Option<String>> {
    Ok(store
        .conn()
        .query_row(
            "SELECT o.id FROM comparisons c JOIN comparisons o
                ON o.experiment = c.experiment AND o.candidate = c.candidate
              WHERE c.id = ?2 AND c.experiment = ?1 AND c.baseline = 'baseline'
                AND o.id <> c.id AND o.status = 'decided' AND o.verdict = 'accept'
                AND json_extract(o.result, '$.revalidates') IS NULL
                AND COALESCE(o.ended_ms, 0) < ?3
              ORDER BY o.ended_ms DESC, o.rowid DESC LIMIT 1",
            params![exp, comparison, before_ms],
            |r| r.get(0),
        )
        .optional()?)
}

/// The raw p of each hypothesis already decided in an experiment (its Holm
/// family), other than `comparison`: revalidations are not hypotheses of
/// their own, and the one a replication stands in for is left out.
fn family(
    store: &Store,
    exp: &str,
    comparison: &str,
    before_ms: i64,
    replicates: Option<&str>,
) -> Result<Vec<f64>> {
    let mut stmt = store.conn().prepare(
        "SELECT json_extract(result, '$.primary.p') FROM comparisons
          WHERE experiment = ?1 AND status = 'decided' AND id <> ?2
            AND (?3 IS NULL OR id <> ?3)
            AND json_extract(result, '$.revalidates') IS NULL
            AND COALESCE(ended_ms, 0) < ?4",
    )?;
    Ok(stmt
        .query_map(params![exp, comparison, replicates, before_ms], |r| {
            r.get::<_, Option<f64>>(0)
        })?
        .filter_map(|r| r.ok().flatten())
        .collect())
}

fn end_status(store: &Store, comparison: &str, status: &str) -> Result<()> {
    store.conn().execute(
        "UPDATE comparisons SET status = ?2, ended_ms = ?3 WHERE id = ?1",
        params![comparison, status, now_ms() as i64],
    )?;
    Ok(())
}

fn end_failed(
    store: &Store,
    exp: &str,
    comparison: &str,
    candidate: &str,
    reason: &str,
) -> Result<()> {
    let tx = store.conn().unchecked_transaction()?;
    tx.execute(
        "UPDATE comparisons SET status = 'failed', ended_ms = ?2, result = ?3 WHERE id = ?1",
        params![
            comparison,
            now_ms() as i64,
            serde_json::json!({ "reason": reason }).to_string()
        ],
    )?;
    tx.execute(
        "INSERT INTO decisions (experiment, comparison, candidate, decision, reason, created_ms) VALUES (?1, ?2, ?3, 'crash', ?4, ?5)",
        params![exp, comparison, candidate, reason, now_ms() as i64],
    )?;
    tx.commit()?;
    Ok(())
}

/// A comparison that is void (trials tampered with tracked files, left
/// processes behind, or the workload reported the metric a different number
/// of times). When it is a revalidation of the current best, the accept it
/// should have replicated is withdrawn as a failed replication is (ADR
/// 0032): a result that could not be reproduced does not stay accepted.
fn end_void(
    store: &Store,
    exp: &str,
    comparison: &str,
    candidate: &str,
    reason: &str,
) -> Result<()> {
    let best: Option<String> = store
        .conn()
        .query_row(
            "SELECT best FROM experiments WHERE name = ?1",
            params![exp],
            |r| r.get(0),
        )
        .optional()?;
    let orig = replicated(store, exp, comparison, i64::MAX)?;
    let (Some(orig), Some(best)) = (orig, best) else {
        return end_failed(store, exp, comparison, candidate, reason);
    };
    if best != candidate {
        return end_failed(store, exp, comparison, candidate, reason);
    }
    let before: String = store.conn().query_row(
        "SELECT baseline FROM comparisons WHERE id = ?1",
        params![orig],
        |r| r.get(0),
    )?;
    let to = if before != candidate && trusted_reference(store, exp, &before)? {
        before
    } else {
        "baseline".to_owned()
    };
    let reason = format!(
        "{reason}; the revalidation could not replicate the accept of {candidate}, so it is withdrawn and the best is {to} again"
    );
    let tx = store.conn().unchecked_transaction()?;
    tx.execute(
        "UPDATE comparisons SET status = 'failed', ended_ms = ?2, result = ?3 WHERE id = ?1",
        params![
            comparison,
            now_ms() as i64,
            serde_json::json!({ "reason": reason }).to_string()
        ],
    )?;
    tx.execute(
        "INSERT INTO decisions (experiment, comparison, candidate, decision, reason, created_ms) VALUES (?1, ?2, ?3, 'crash', ?4, ?5)",
        params![exp, comparison, candidate, reason, now_ms() as i64],
    )?;
    tx.execute(
        "INSERT INTO best_resets (experiment, from_best, to_best, reason, created_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![exp, candidate, to, format!("revalidation {comparison} was void and did not replicate the accept"), now_ms() as i64],
    )?;
    tx.execute(
        "UPDATE experiments SET best = ?2 WHERE name = ?1",
        params![exp, to],
    )?;
    tx.commit()?;
    Ok(())
}

fn finish(
    store: &Store,
    e: &Experiment,
    comparison: &str,
    candidate: &str,
    verdict: Verdict,
    mut result: serde_json::Value,
) -> Result<()> {
    if verdict == Verdict::Invalid
        && !result["reason"]
            .as_str()
            .unwrap_or("")
            .contains("fingerprint")
    {
        result["reason"] = serde_json::json!("invalid comparison");
    }
    let mut reason = result["reason"].as_str().unwrap_or("").to_owned();
    // An ACCEPT against a reference that is not where the candidate came
    // from measures everything between the two commits (round 9: a slowed
    // down `--baseline` made unchanged code "faster").
    if verdict == Verdict::Accept
        && let Ok((b, c)) = store.conn().query_row(
            "SELECT baseline, candidate FROM comparisons WHERE id = ?1",
            params![comparison],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        // Kept variants are snapshots the next candidate does not descend
        // from; only the original baseline must be an ancestor.
        && b == "baseline"
        && let (Ok(bv), Ok(cv)) = (variant(store, &e.name, &b), variant(store, &e.name, &c))
        && let (Some(bs), Some(cs)) = (&bv.commit_sha, &cv.commit_sha)
        && !Command::new("git")
            .args(["merge-base", "--is-ancestor", bs, cs])
            .current_dir(store.root())
            .status()
            .is_ok_and(|st| st.success())
    {
        reason.push_str(&format!(
            "; warning: the reference {b} ({}) is not an ancestor of the candidate, so the difference includes everything between the two commits, not only the change",
            &bs[..bs.len().min(12)]
        ));
        result["reason"] = serde_json::json!(reason.clone());
    }
    // A revalidation (the kept best, again, against the original baseline)
    // says so in the decision log.
    let base: String = store.conn().query_row(
        "SELECT baseline FROM comparisons WHERE id = ?1",
        params![comparison],
        |r| r.get(0),
    )?;
    let kept_before: bool = store.conn().query_row(
        "SELECT EXISTS (SELECT 1 FROM decisions WHERE experiment = ?1 AND candidate = ?2 AND decision = 'keep')",
        params![e.name, candidate],
        |r| r.get(0),
    )?;
    if kept_before && base == "baseline" {
        reason = format!("revalidation: {reason}");
    }
    // Beating a reference other than the current best does not displace
    // the best (round 10: a candidate accepted `--against baseline` replaced
    // a much faster best).
    let becomes_best = verdict == Verdict::Accept && (base == e.best || e.best == candidate);
    if verdict == Verdict::Accept && !becomes_best {
        reason.push_str(&format!(
            "; the best stays {}: this was compared with {base}, not with the best",
            e.best
        ));
    }
    // A replication that does not hold withdraws the accept it replicates
    // (ADR 0032): the best goes back to what the accept was measured against.
    let mut withdrawn_to: Option<String> = None;
    if matches!(verdict, Verdict::Reject | Verdict::Inconclusive)
        && e.best == candidate
        && let Some(orig) = result["revalidates"].as_str()
    {
        let before: String = store.conn().query_row(
            "SELECT baseline FROM comparisons WHERE id = ?1",
            params![orig],
            |r| r.get(0),
        )?;
        let to = if before != candidate && trusted_reference(store, &e.name, &before)? {
            before
        } else {
            "baseline".to_owned()
        };
        reason.push_str(&format!(
            "; the replication did not hold, so the accept of {candidate} is withdrawn and the best is {to} again"
        ));
        withdrawn_to = Some(to);
    }
    let tx = store.conn().unchecked_transaction()?;
    tx.execute(
        "UPDATE comparisons SET status = 'decided', verdict = ?2, result = ?3, ended_ms = ?4 WHERE id = ?1",
        params![comparison, verdict.as_str(), result.to_string(), now_ms() as i64],
    )?;
    if let Some(to) = &withdrawn_to {
        tx.execute(
            "INSERT INTO best_resets (experiment, from_best, to_best, reason, created_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![e.name, candidate, to, format!("revalidation {comparison} did not replicate the accept"), now_ms() as i64],
        )?;
        tx.execute(
            "UPDATE experiments SET best = ?2 WHERE name = ?1",
            params![e.name, to],
        )?;
    }
    let decision = if verdict == Verdict::Accept {
        "keep"
    } else {
        // Say what it was compared against (round 14).
        reason = format!("vs {base}: {reason}");
        "discard"
    };
    tx.execute(
        "INSERT INTO decisions (experiment, comparison, candidate, decision, reason, created_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![e.name, comparison, candidate, decision, reason, now_ms() as i64],
    )?;
    if becomes_best {
        tx.execute(
            "UPDATE experiments SET best = ?2 WHERE name = ?1",
            params![e.name, candidate],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Whether a variant may serve as a reference: the original baseline, or
/// one an ACCEPT kept and no `exp best` reset turned away from.
fn trusted_reference(store: &Store, exp: &str, name: &str) -> Result<bool> {
    if name == "baseline" {
        return Ok(true);
    }
    let kept: bool = store.conn().query_row(
        "SELECT EXISTS (SELECT 1 FROM decisions WHERE experiment = ?1 AND candidate = ?2 AND decision = 'keep')",
        params![exp, name],
        |r| r.get(0),
    )?;
    let reset_away: bool = store.conn().query_row(
        "SELECT EXISTS (SELECT 1 FROM best_resets WHERE experiment = ?1 AND from_best = ?2)",
        params![exp, name],
        |r| r.get(0),
    )?;
    Ok(kept && !reset_away)
}

/// Sets the best variant by hand (after an ACCEPT found wrong), with the
/// reason recorded next to the decision log.
pub fn reset_best(store: &Store, exp: &str, to: &str, reason: &str) -> Result<String> {
    let e = get(store, exp)?;
    if !existing_variant(store, exp, to)? {
        return refused(format!("no variant {to:?} in experiment {exp}"));
    }
    if !trusted_reference(store, exp, to)? {
        return refused(format!(
            "{to:?} cannot be the best: only the baseline or a variant that was kept (ACCEPT) and not reset away from"
        ));
    }
    let tx = store.conn().unchecked_transaction()?;
    tx.execute(
        "INSERT INTO best_resets (experiment, from_best, to_best, reason, created_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![exp, e.best, to, reason, now_ms() as i64],
    )?;
    tx.execute(
        "UPDATE experiments SET best = ?2 WHERE name = ?1",
        params![exp, to],
    )?;
    tx.commit()?;
    Ok(e.best)
}

// ------------------------------------------------------------ reports

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub experiment: String,
    pub hypothesis: String,
    pub protocol_hash: String,
    /// The frozen protocol itself, so nothing about it is hidden.
    pub protocol: Protocol,
    pub best: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comparison: Option<ComparisonReport>,
    pub decisions: Vec<serde_json::Value>,
    /// How to read the kept logs of the latest comparison, when it failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logs: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComparisonReport {
    pub id: String,
    pub baseline: String,
    pub candidate: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    pub baseline_trials: u32,
    pub candidate_trials: u32,
    pub target_trials: u32,
    pub max_trials: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
}

/// Comparisons of an experiment that reached a verdict (its Holm family).
pub fn decided_comparisons(store: &Store, exp: &str) -> Result<usize> {
    let n: i64 = store.conn().query_row(
        "SELECT COUNT(*) FROM comparisons WHERE experiment = ?1 AND status = 'decided'",
        params![exp],
        |r| r.get(0),
    )?;
    Ok(n as usize)
}

pub fn latest_comparison(store: &Store, exp: &str) -> Result<Option<String>> {
    Ok(store
        .conn()
        .query_row(
            "SELECT id FROM comparisons WHERE experiment = ?1 ORDER BY created_ms DESC, rowid DESC LIMIT 1",
            params![exp],
            |r| r.get(0),
        )
        .optional()?)
}

pub fn comparison_report(store: &Store, e: &Experiment, id: &str) -> Result<ComparisonReport> {
    let (baseline, candidate, status, verdict, result, worker): (String, String, String, Option<String>, Option<String>, Option<i64>) =
        store.conn().query_row(
            "SELECT baseline, candidate, status, verdict, result, worker_pid FROM comparisons WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )?;
    // A worker that died leaves the comparison failed, not running forever.
    if status == "running"
        && let Some(pid) = worker
        && !process::alive(pid as u32)
    {
        end_failed(
            store,
            &e.name,
            id,
            &candidate,
            "experiment worker exited without deciding",
        )?;
        return comparison_report(store, e, id);
    }
    let count = |arm: &str| -> Result<u32> {
        Ok(store.conn().query_row(
            "SELECT COUNT(*) FROM trials WHERE comparison = ?1 AND arm = ?2 AND warmup = 0",
            params![id, arm],
            |r| r.get::<_, i64>(0),
        )? as u32)
    };
    Ok(ComparisonReport {
        id: id.to_owned(),
        baseline,
        candidate,
        status,
        verdict,
        baseline_trials: count("baseline")?,
        candidate_trials: count("candidate")?,
        target_trials: e.protocol.trials,
        max_trials: e.protocol.max_trials,
        result: result.and_then(|r| serde_json::from_str(&r).ok()),
    })
}

pub fn report(store: &Store, exp: &str) -> Result<Report> {
    report_at(store, exp, None)
}

/// The comparison `id` of an experiment, refused when it is not one of its.
pub fn check_comparison(store: &Store, exp: &str, id: &str) -> Result<()> {
    let known: bool = store.conn().query_row(
        "SELECT EXISTS (SELECT 1 FROM comparisons WHERE id = ?1 AND experiment = ?2)",
        params![id, exp],
        |r| r.get(0),
    )?;
    if known {
        Ok(())
    } else {
        refused(format!("experiment {exp} has no comparison {id}"))
    }
}

/// `report`, for one comparison (the latest when `None`).
pub fn report_at(store: &Store, exp: &str, at: Option<&str>) -> Result<Report> {
    let e = get(store, exp)?;
    let id = match at {
        Some(id) => {
            check_comparison(store, exp, id)?;
            Some(id.to_owned())
        }
        None => latest_comparison(store, exp)?,
    };
    let comparison = match &id {
        Some(id) => Some(comparison_report(store, &e, id)?),
        None => None,
    };
    let mut stmt = store.conn().prepare(
        "SELECT comparison, candidate, decision, reason FROM (
           SELECT comparison, candidate, decision, reason, created_ms FROM decisions WHERE experiment = ?1
           UNION ALL
           SELECT '-', to_best, 'reset', 'best set by hand from ' || from_best || ': ' || reason, created_ms
             FROM best_resets WHERE experiment = ?1)
         ORDER BY created_ms",
    )?;
    let decisions = stmt
        .query_map(params![exp], |r| {
            Ok(serde_json::json!({
                "comparison": r.get::<_, String>(0)?,
                "candidate": r.get::<_, String>(1)?,
                "decision": r.get::<_, String>(2)?,
                "reason": r.get::<_, String>(3)?,
            }))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let e = get(store, exp)?; // best may have changed
    let logs = comparison
        .as_ref()
        .filter(|c| {
            c.status == "failed"
                || c.result
                    .as_ref()
                    .is_some_and(|r| r["specification"] == "violated")
        })
        .map(|c| match at {
            Some(_) => format!("prova exp log {exp} --comparison {}", c.id),
            None => format!("prova exp log {exp}"),
        });
    Ok(Report {
        experiment: e.name,
        hypothesis: e.hypothesis,
        protocol_hash: e.protocol_hash,
        protocol: e.protocol,
        best: e.best,
        comparison,
        decisions,
        logs,
    })
}

/// Blocks at most `max_wait` until the latest comparison ends.
pub fn wait(store: &Store, exp: &str, max_wait: Duration) -> Result<Report> {
    let deadline = Instant::now() + max_wait;
    loop {
        let r = report(store, exp)?;
        let done = r.comparison.as_ref().is_none_or(|c| c.status != "running");
        if done || Instant::now() >= deadline {
            return Ok(r);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

use std::os::unix::process::CommandExt;

#[cfg(test)]
mod tests {
    #[test]
    fn oracle_failure_quotes_the_failing_test_not_cargos_trailer() {
        let log = "running 2 tests\ntest ok_one ... ok\ntest count_primes_small_values ... FAILED\n\nfailures:\n\n---- count_primes_small_values stdout ----\nthread 'x' panicked at tests/primes.rs:9:5:\nassertion `left == right` failed\n  left: 1\n right: 0\n\nfailures:\n    count_primes_small_values\n\ntest result: FAILED. 1 passed; 1 failed\nerror: test failed, to rerun pass `--test primes`\n";
        let q = failing_test_lines(log).unwrap();
        assert!(q.contains("count_primes_small_values stdout"), "{q}");
        assert!(q.contains("panicked at tests/primes.rs"), "{q}");
        assert!(q.contains("assertion"), "{q}");
        assert!(!q.contains("to rerun"), "{q}");
        assert_eq!(
            failing_test_lines("test a ... ok\ntest b ... FAILED\n").unwrap(),
            "test b ... FAILED"
        );
    }

    #[test]
    fn large_improvements_also_read_as_a_ratio() {
        assert_eq!(
            ratio_interval(Better::Lower, 0.916, 1.0),
            " = at least 11.9x better"
        );
        assert_eq!(
            ratio_interval(Better::Lower, 0.9, 0.95),
            " = 10.0x to 20.0x better"
        );
        assert_eq!(ratio_interval(Better::Lower, 0.1, 0.3), "");
        assert_eq!(
            ratio_interval(Better::Higher, 1.0, 3.0),
            " = 2.0x to 4.0x better"
        );
    }

    use super::decisive_line;

    #[test]
    fn the_decisive_oracle_line_skips_generic_trailers() {
        let t = "CountPrimes(2) = 1, specification says 0\nexit status 1\nFAIL\nmake: *** [Makefile:3: conform] Error 1\nError: Process completed with exit code 2.\n";
        assert_eq!(
            decisive_line(t),
            Some("CountPrimes(2) = 1, specification says 0")
        );
        assert_eq!(decisive_line("only\nexit status 1\n"), Some("only"));
        assert_eq!(decisive_line("exit status 1\n"), Some("exit status 1"));
        assert_eq!(decisive_line("\n\n"), None);
    }

    use super::*;

    #[test]
    fn interleaves_and_refuses_early_verdicts() {
        let mut c = Comparison::new(5, 6);
        assert_eq!(
            c.next_arm(),
            None,
            "no trial before the specification check"
        );
        assert!(c.check_spec(Arm::Baseline, true) && c.check_spec(Arm::Candidate, true));
        let arms: Vec<Arm> = (0..4).map(|_| c.trial("m").unwrap()).collect();
        // ABBA: pairs alternate which arm goes first (ADR 0024).
        assert_eq!(
            arms,
            vec![Arm::Baseline, Arm::Candidate, Arm::Candidate, Arm::Baseline]
        );
        assert!(!c.can_change_protocol());
        assert_eq!(c.decide(Verdict::Accept), None, "peeking refused");
        while c.cand < 5 {
            c.trial("m");
        }
        assert_eq!(c.decide(Verdict::Accept), Some(Verdict::Accept));
        assert_eq!(c.next_arm(), None);
    }

    #[test]
    fn mixed_machines_are_invalid_and_max_inconclusive_rejects() {
        let mut c = Comparison::new(5, 5);
        c.check_spec(Arm::Baseline, true);
        c.check_spec(Arm::Candidate, true);
        for i in 0..10 {
            c.trial(if i == 3 { "other" } else { "m" });
        }
        assert_eq!(c.decide(Verdict::Accept), Some(Verdict::Invalid));
        let mut c = Comparison::new(5, 5);
        c.check_spec(Arm::Baseline, true);
        c.check_spec(Arm::Candidate, true);
        for _ in 0..10 {
            c.trial("m");
        }
        assert_eq!(c.decide(Verdict::Inconclusive), Some(Verdict::Reject));
    }

    #[test]
    fn a_spec_violator_is_rejected_unmeasured() {
        let mut c = Comparison::new(5, 5);
        assert!(c.check_spec(Arm::Baseline, true));
        assert!(c.check_spec(Arm::Candidate, false));
        assert!(!c.check_spec(Arm::Candidate, true), "checked once");
        assert_eq!(c.next_arm(), None);
        assert_eq!(c.reject_violation(), Some(Verdict::Reject));
        let mut b = Comparison::new(5, 5);
        b.check_spec(Arm::Baseline, false);
        b.check_spec(Arm::Candidate, true);
        assert_eq!((b.next_arm(), b.reject_violation()), (None, None));
    }

    fn result(improvement_ci: (f64, f64), p: f64) -> MetricResult {
        let mut r = measure(
            "t",
            Better::Lower,
            &[10.0, 11.0, 12.0, 13.0, 14.0],
            &[5.0, 6.0, 7.0, 8.0, 9.0],
            0.05,
            None,
        );
        r.improvement_ci = improvement_ci;
        r.p_adjusted = p;
        r.p = p;
        r
    }

    #[test]
    fn judge_applies_the_minimum_effect() {
        assert_eq!(
            judge(&result((0.05, 0.2), 0.01), &[], 0.02, 0.05).0,
            Verdict::Accept
        );
        assert_eq!(
            judge(&result((0.01, 0.2), 0.01), &[], 0.02, 0.05).0,
            Verdict::Inconclusive
        );
        assert_eq!(
            judge(&result((-0.2, -0.05), 0.01), &[], 0.02, 0.05).0,
            Verdict::Reject
        );
        assert_eq!(
            judge(&result((-0.01, 0.015), 0.4), &[], 0.02, 0.05).0,
            Verdict::Reject
        );
        let bad_guard = result((-0.5, -0.3), 0.01);
        assert_eq!(
            judge(&result((0.05, 0.2), 0.01), &[bad_guard], 0.02, 0.05).0,
            Verdict::Reject
        );
    }

    #[test]
    fn improvement_sign_follows_direction() {
        let r = measure(
            "t",
            Better::Lower,
            &[10.0, 11.0, 12.0, 13.0, 14.0],
            &[5.0, 6.0, 7.0, 8.0, 9.0],
            0.05,
            None,
        );
        assert!(r.improvement > 0.0 && r.improvement_ci.0 > 0.0);
        let r = measure(
            "t",
            Better::Higher,
            &[10.0, 11.0, 12.0, 13.0, 14.0],
            &[5.0, 6.0, 7.0, 8.0, 9.0],
            0.05,
            None,
        );
        assert!(r.improvement < 0.0);
    }
}

/// ADR 0032: a revalidation replicates a hypothesis; it adds no comparison to
/// the Holm family, and a failed replication withdraws the accept.
#[cfg(test)]
mod revalidation_tests {
    use super::*;

    fn protocol() -> Protocol {
        Protocol {
            metric: "t".into(),
            unit: None,
            better: Better::Lower,
            min_effect: 0.02,
            alpha: 0.05,
            trials: 5,
            min_trials: 5,
            max_trials: 5,
            warmup: 0,
            guardrails: Vec::new(),
            command: vec!["true".into()],
            kill_after_ms: 1000,
            setup: None,
            specs: Vec::new(),
            oracle: None,
            scope: Vec::new(),
            scope_only: false,
        }
    }

    fn fresh() -> (Store, PathBuf) {
        let d = std::env::temp_dir().join(format!(
            "prova-reval-{}-{}",
            std::process::id(),
            crate::ids::new_run_id()
        ));
        std::fs::create_dir_all(&d).unwrap();
        let s = Store::open_dir(&d, &d.join(".prova")).unwrap();
        let p = protocol();
        let c = s.conn();
        c.execute(
            "INSERT INTO experiments (name, hypothesis, protocol, protocol_hash, cwd, created_ms, best)
             VALUES ('e', 'h', ?1, ?2, '', 1, 'c1')",
            params![serde_json::to_string(&p).unwrap(), p.hash()],
        )
        .unwrap();
        for v in ["baseline", "c1"] {
            c.execute(
                "INSERT INTO variants (experiment, name, kind, code, params, created_ms)
                 VALUES ('e', ?1, 'workdir', 'x', '{}', 1)",
                params![v],
            )
            .unwrap();
        }
        (s, d)
    }

    /// ADR 0037: `--allow-network` is gone, but a protocol stored with it
    /// still loads, with the hash it was frozen with.
    #[test]
    fn a_protocol_stored_with_allow_network_still_loads() {
        let (s, _d) = fresh();
        let mut v = serde_json::to_value(protocol()).unwrap();
        v["allow_network"] = serde_json::json!(true);
        s.conn()
            .execute(
                "UPDATE experiments SET protocol = ?1, protocol_hash = 'frozen-hash' WHERE name = 'e'",
                params![v.to_string()],
            )
            .unwrap();
        assert_eq!(get(&s, "e").unwrap().protocol_hash, "frozen-hash");
    }

    /// A comparison of c1 against the baseline with five samples per arm.
    fn comparison(s: &Store, id: &str, decided: Option<(&str, serde_json::Value)>, cand: [f64; 5]) {
        let c = s.conn();
        let (status, verdict, result, ended) = match &decided {
            Some((v, r)) => ("decided", Some(*v), Some(r.to_string()), Some(100)),
            None => ("running", None, None, None),
        };
        let created = if decided.is_some() { 10 } else { 200 };
        c.execute(
            "INSERT INTO comparisons (id, experiment, baseline, candidate, status, verdict, result, created_ms, ended_ms)
             VALUES (?1, 'e', 'baseline', 'c1', ?2, ?3, ?4, ?5, ?6)",
            params![id, status, verdict, result, created, ended],
        )
        .unwrap();
        let base = [10.0, 11.0, 12.0, 13.0, 14.0];
        let rows = base
            .iter()
            .map(|v| ("baseline", *v))
            .zip(cand.iter().map(|v| ("candidate", *v)))
            .flat_map(|(a, b)| [a, b]);
        for (i, (arm, v)) in rows.enumerate() {
            let seq = i as i64 + 1;
            c.execute(
                "INSERT INTO trials (comparison, seq, arm, warmup, state, exit_code, fingerprint, started_ms, ended_ms)
                 VALUES (?1, ?2, ?3, 0, 'complete', 0, 'f', 1, 2)",
                params![id, seq, arm],
            )
            .unwrap();
            c.execute(
                "INSERT INTO samples (comparison, seq, metric, value) VALUES (?1, ?2, 't', ?3)",
                params![id, seq, v],
            )
            .unwrap();
        }
    }

    const FASTER: [f64; 5] = [1.0, 2.0, 3.0, 4.0, 5.0];
    const SAME: [f64; 5] = [10.5, 11.5, 12.5, 13.5, 14.5];

    fn decided_accept(revalidates: bool) -> (&'static str, serde_json::Value) {
        let mut r = serde_json::json!({ "primary": { "p": 0.0079 } });
        if revalidates {
            r["revalidates"] = serde_json::json!("c-1");
        }
        ("accept", r)
    }

    #[test]
    fn a_revalidation_adds_no_comparison_to_the_family() {
        let (s, _d) = fresh();
        comparison(&s, "c-1", Some(decided_accept(false)), FASTER);
        // Six earlier revalidations, each with the smallest p the test allows.
        for i in 0..6 {
            let id = format!("rv-{i}");
            comparison(&s, &id, Some(decided_accept(true)), FASTER);
        }
        comparison(&s, "rv-new", None, FASTER);
        let e = get(&s, "e").unwrap();
        let (verdict, result) = evaluate(&s, &e, "rv-new").unwrap();
        assert_eq!(result["comparisons_in_family"], 1, "{result}");
        assert_eq!(result["revalidates"], "c-1", "{result}");
        assert_eq!(verdict, Verdict::Accept, "{result}");
    }

    #[test]
    fn a_spec_frozen_file_is_not_offered_for_change() {
        let mut p = protocol();
        p.specs = vec![SpecFile {
            path: "spec/x.qnt".into(),
            sha256: "0".into(),
        }];
        let both = out_of_scope_why(&p, &["spec/x.qnt".into(), "tests/a.rs".into()]);
        assert!(
            both.contains("spec/x.qnt is frozen as the specification"),
            "{both}"
        );
        assert!(!both.contains("--change"), "{both}");
        let other = out_of_scope_why(&p, &["tests/a.rs".into()]);
        assert!(
            other.contains("allow them with `--change <path>`"),
            "{other}"
        );
    }

    /// The candidate's own misbehaviour in a revalidation withdraws it.
    #[test]
    fn a_void_revalidation_of_the_candidates_own_making_withdraws_the_accept() {
        let (s, _d) = fresh();
        comparison(&s, "c-1", Some(decided_accept(false)), FASTER);
        comparison(&s, "rv", None, SAME);
        end_void(&s, "e", "rv", "c1", "it left a process behind").unwrap();
        assert_eq!(get(&s, "e").unwrap().best, "baseline");
    }

    /// A worker's scratch root is removed even when the workload left
    /// read-only directories in it (a Go module cache).
    #[test]
    fn a_read_only_tree_is_still_removed() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("prova-rotree-{}", crate::ids::new_run_id()));
        let inner = d.join("a/b");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("f"), "x").unwrap();
        for p in [&inner, &d.join("a")] {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o555)).unwrap();
        }
        remove_tree(&d).unwrap();
        assert!(!d.exists());
    }

    #[test]
    fn a_failed_replication_withdraws_the_accept() {
        let (s, _d) = fresh();
        comparison(&s, "c-1", Some(decided_accept(false)), FASTER);
        comparison(&s, "rv", None, SAME);
        let e = get(&s, "e").unwrap();
        let (verdict, result) = evaluate(&s, &e, "rv").unwrap();
        assert_ne!(verdict, Verdict::Accept, "{result}");
        finish(&s, &e, "rv", "c1", verdict, result).unwrap();
        assert_eq!(get(&s, "e").unwrap().best, "baseline");
        let resets: i64 = s
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM best_resets WHERE experiment = 'e' AND from_best = 'c1' AND to_best = 'baseline'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(resets, 1);
    }
}

/// ADR 0032: files an oracle reads as expected values or build steps decide
/// the verdict too; a candidate that edits them is out of scope.
#[cfg(test)]
mod oracle_input_tests {
    use super::*;

    #[test]
    fn the_oracles_expected_data_and_build_scripts_are_harness_files() {
        for f in [
            "testdata/expected.json",
            "pkg/testdata/primes.golden",
            "tests/fixtures/input.csv",
            "fixtures/answer.txt",
            "src/__snapshots__/a.snap",
            "pkg/a.golden",
            "golden/out.txt",
            "build.rs",
            "crates/x/build.rs",
        ] {
            assert!(harness_file(f), "{f} should decide the verdict");
        }
        assert!(!harness_file("src/primes.rs"));
        assert!(!harness_file("src/golden_ratio.rs"));
    }
}
