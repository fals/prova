//! Experiments (ADR 0007): a frozen protocol, interleaved trials of a
//! baseline and a candidate, a rank-based verdict, and a decision log for
//! improvement loops. `Comparison` mirrors `spec/experiment.qnt`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rusqlite::{OptionalExtension, params};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Better {
    Lower,
    Higher,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Guardrail {
    pub metric: String,
    pub better: Better,
}

/// Declared before any data; frozen at the first trial.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Trials and oracle may use the network (a sandboxed arm has only
    /// loopback otherwise).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_network: bool,
}

/// A specification file: path relative to the repository root, sha256.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
        "[{}, {}{}], positive = better",
        pct(lo),
        pct(hi),
        if capped {
            " (upper bound cut at 100%: the interval of differences reaches the whole baseline median, i.e. the candidate is far better, not that it takes no time)"
        } else {
            ""
        }
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
                "no practical effect, neither better nor worse beyond ±{0}: CI {ci} inside ±{0}",
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
        .env("GIT_NO_REPLACE_OBJECTS", "1")
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
                .env("GIT_NO_REPLACE_OBJECTS", "1")
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
            .env("GIT_NO_REPLACE_OBJECTS", "1")
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
                    .env("GIT_NO_REPLACE_OBJECTS", "1")
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
    /// CPU (ms) other processes on the machine used during the trial.
    foreign_ms: f64,
    /// CPU (ms) the trial's own processes used.
    own_ms: f64,
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
                    .env("GIT_NO_REPLACE_OBJECTS", "1")
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
            // The checkout must hold the commit's own bytes: a local smudge
            // filter or a `git replace` changes what runs without changing
            // the commit (round 11).
            if let Some(path) = checkout_differs(&wt) {
                return refused(format!(
                    "the checkout of {} differs from commit {}'s own content at {path} (a local git filter, or a replaced object?): refusing to measure it",
                    v.name,
                    &sha[..12]
                ));
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
    let mut cmd = sandboxed(dir, &p.command[0], &p.command[1..], p.allow_network);
    cmd.current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // The workload must not reach prova's own store (round 9).
        .env_remove("PROVA_HOME")
        .env("PYTHONNOUSERSITE", "1")
        .env("PYTHONPYCACHEPREFIX", pycache_dir(dir))
        .process_group(0);
    for (k, val) in &v.params {
        cmd.env(format!("PROVA_PARAM_{}", k.to_ascii_uppercase()), val);
    }
    let cpu_before = process::cpu_ticks();
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
                foreign_ms: 0.0,
                own_ms: 0.0,
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
    let cpu_after = process::cpu_ticks();
    let foreign_ms = (cpu_after.0.saturating_sub(cpu_before.0) as f64
        - cpu_after.1.saturating_sub(cpu_before.1) as f64)
        .max(0.0)
        * 10.0;
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
        foreign_ms,
        own_ms: cpu_after.1.saturating_sub(cpu_before.1) as f64 * 10.0,
    })
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
                    .env("GIT_NO_REPLACE_OBJECTS", "1")
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
    // CPU other processes used per trial, per arm, with the trial's wall time.
    let foreign: std::cell::RefCell<[Vec<(f64, f64)>; 2]> = Default::default();
    // The trial's own CPU, with its wall time: a process stopped from
    // outside (SIGSTOP) takes wall time but no CPU (round 11).
    let own: std::cell::RefCell<[Vec<(f64, f64)>; 2]> = Default::default();
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
            foreign.borrow_mut()[i].push((r.foreign_ms, r.wall_ms));
            own.borrow_mut()[i].push((r.own_ms, r.wall_ms));
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
            let ok = sandboxed(&dirs[i], "sh", &["-c".to_owned(), setup.clone()], true)
                .env_remove("PROVA_HOME")
                .env("PYTHONNOUSERSITE", "1")
                .env("PYTHONPYCACHEPREFIX", pycache_dir(&dirs[i]))
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
    // Snapshot checkouts are prova's own: make their tracked files read-only
    // too, so a rewrite fails instead of being restored before the check.
    for (i, (_, v)) in arms.iter().enumerate() {
        if v.commit_sha.is_some() && dirs[i] != store.root() {
            read_only_tracked(&dirs[i]);
        }
    }
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
        if let Some(first) = outside.first() {
            let why = if p.scope_only {
                format!(
                    "candidate changes {} file(s) outside the protocol's --change-only scope, e.g. {first}",
                    outside.len()
                )
            } else {
                format!(
                    "candidate changes {} file(s) that decide the verdict (tests, benchmarks, build and runner files), e.g. {first}; if the change needs them, allow them with `--change <path>` (`prova exp protocol` before the first trial, or a new experiment)",
                    outside.len()
                )
            };
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
                let why = leaked.borrow().clone().map_or_else(
                    || format!("{} warmup trial failed", arm.as_str()),
                    |l| format!("{l} after it ended; it would disturb the other arm's trials (`prova exp log {exp}`)"),
                );
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
                let reason = leaked.borrow().clone().map_or_else(
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
                return end_failed(store, &exp, comparison, &cand_name, &reason);
            }
            if let Some(where_) = tampered() {
                let reason = format!(
                    "trial {} changed tracked files in the {where_}: a workload must not rewrite code or data; the comparison is void",
                    counter.get()
                );
                return end_failed(store, &exp, comparison, &cand_name, &reason);
            }
            c.trial(&fingerprint_of(store, comparison, counter.get())?);
        }
        // Load from outside the trials must not favour one arm: interleaving
        // spreads background noise evenly, so a large difference is
        // something aimed at one arm (round 9: systemd-run burner).
        if let Some(reason) = {
            let f = foreign.borrow();
            let o = own.borrow();
            load_imbalance(&f[0], &f[1]).or_else(|| stopped_arm(&o[0], &o[1]))
        } {
            return end_failed(store, &exp, comparison, &cand_name, &reason);
        }
        // Each arm against its own earlier measurements: a variant whose
        // median moved 2x or more since its earlier comparisons was disturbed
        // (round 10: the other arm was slowed through Python's user site
        // and a planted .pyc), or its workload changed.
        if let Some(reason) = drifted(
            store,
            &exp,
            comparison,
            &p.metric,
            [&arms[0].0, &arms[1].0],
            [&base_name_for(store, comparison)?, &cand_name],
        )? {
            return end_failed(store, &exp, comparison, &cand_name, &reason);
        }
        // Each arm's workload must report the metric as often per trial as
        // the other: extra lines are forged output (a candidate's code can
        // print benchmark lines), missing ones a changed workload (round 8).
        {
            let l = lines.borrow();
            let usual = |v: &Vec<usize>| v.iter().copied().max().unwrap_or(0);
            let (b, c) = (usual(&l[0]), usual(&l[1]));
            if b != c || l[1].iter().any(|n| *n != c) {
                let reason = format!(
                    "the candidate's workload reported {} {}x per trial, the baseline's {b}x: extra lines are forged output, missing ones a changed workload; the comparison is void",
                    p.metric, c
                );
                return end_failed(store, &exp, comparison, &cand_name, &reason);
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
        if p.alpha > 0.1 {
            caveats.push(format!("alpha {} is weak (0.05 is usual)", p.alpha));
        }
        if p.min_effect < 0.01 {
            caveats.push(format!(
                "a minimum effect of {:.1}% lets noise count as an improvement",
                p.min_effect * 100.0
            ));
        }
        if let Some(sha) = &arms[0].1.commit_sha {
            let others: i64 = store.conn().query_row(
                "SELECT COUNT(DISTINCT x.name) FROM experiments x JOIN variants v ON v.experiment = x.name
                  WHERE v.name = 'baseline' AND v.commit_sha = ?1 AND x.name <> ?2
                    AND json_extract(x.protocol, '$.metric') = ?3",
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
        if !process::MACHINE_COUNTERS {
            caveats.push(
                "this OS gives prova no CPU counters for the machine or for finished children, and no subreaper: load from other processes, an arm stopped from outside, and processes that left a trial's or oracle's process group were not checked".into(),
            );
        }
        if !caveats.is_empty() {
            result["caveats"] = serde_json::json!(caveats);
        }
        // Say how isolated the arms were (ADR 0024).
        result["sandbox"] = serde_json::json!(if sandbox_available() {
            "bubblewrap: each arm could write only its own checkout and its private state and caches, with no network unless the protocol allows it"
        } else {
            "none: bubblewrap is not available here, so the arms ran as the same user with access to each other's files"
        });
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
    "**/Makefile",
    "**/GNUmakefile",
    "**/*.mk",
    "prova.toml",
    "**/package.json",
    "**/pytest.ini",
    "**/.pytest.ini",
    // Python startup hooks that run in every interpreter (round 10)
    "**/sitecustomize.py",
    "**/usercustomize.py",
    "**/*.pth",
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

/// Python's standard library top-level modules: a file or package of that
/// name in the checkout shadows the real one for code run from it (round
/// 10: a `random.py` made the oracle's random sample constant).
const PYTHON_STDLIB: &[&str] = &[
    "abc",
    "annotationlib",
    "antigravity",
    "argparse",
    "array",
    "ast",
    "asyncio",
    "atexit",
    "base64",
    "bdb",
    "binascii",
    "bisect",
    "builtins",
    "bz2",
    "cProfile",
    "calendar",
    "cmath",
    "cmd",
    "code",
    "codecs",
    "codeop",
    "collections",
    "colorsys",
    "compileall",
    "compression",
    "concurrent",
    "configparser",
    "contextlib",
    "contextvars",
    "copy",
    "copyreg",
    "csv",
    "ctypes",
    "curses",
    "dataclasses",
    "datetime",
    "dbm",
    "decimal",
    "difflib",
    "dis",
    "doctest",
    "email",
    "encodings",
    "ensurepip",
    "enum",
    "errno",
    "faulthandler",
    "fcntl",
    "filecmp",
    "fileinput",
    "fnmatch",
    "fractions",
    "ftplib",
    "functools",
    "gc",
    "genericpath",
    "getopt",
    "getpass",
    "gettext",
    "glob",
    "graphlib",
    "grp",
    "gzip",
    "hashlib",
    "heapq",
    "hmac",
    "html",
    "http",
    "idlelib",
    "imaplib",
    "importlib",
    "inspect",
    "io",
    "ipaddress",
    "itertools",
    "json",
    "keyword",
    "linecache",
    "locale",
    "logging",
    "lzma",
    "mailbox",
    "marshal",
    "math",
    "mimetypes",
    "mmap",
    "modulefinder",
    "msvcrt",
    "multiprocessing",
    "netrc",
    "nt",
    "ntpath",
    "nturl2path",
    "numbers",
    "opcode",
    "operator",
    "optparse",
    "os",
    "pathlib",
    "pdb",
    "pickle",
    "pickletools",
    "pkgutil",
    "platform",
    "plistlib",
    "poplib",
    "posix",
    "posixpath",
    "pprint",
    "profile",
    "profiling",
    "pstats",
    "pty",
    "pwd",
    "py_compile",
    "pyclbr",
    "pydoc",
    "pydoc_data",
    "pyexpat",
    "queue",
    "quopri",
    "random",
    "re",
    "readline",
    "reprlib",
    "resource",
    "rlcompleter",
    "runpy",
    "sched",
    "secrets",
    "select",
    "selectors",
    "shelve",
    "shlex",
    "shutil",
    "signal",
    "site",
    "smtplib",
    "socket",
    "socketserver",
    "sqlite3",
    "ssl",
    "stat",
    "statistics",
    "string",
    "stringprep",
    "struct",
    "subprocess",
    "symtable",
    "sys",
    "sysconfig",
    "syslog",
    "tabnanny",
    "tarfile",
    "tempfile",
    "termios",
    "textwrap",
    "this",
    "threading",
    "time",
    "timeit",
    "tkinter",
    "token",
    "tokenize",
    "tomllib",
    "trace",
    "traceback",
    "tracemalloc",
    "tty",
    "turtle",
    "turtledemo",
    "types",
    "typing",
    "unicodedata",
    "unittest",
    "urllib",
    "uuid",
    "venv",
    "warnings",
    "wave",
    "weakref",
    "webbrowser",
    "winreg",
    "winsound",
    "wsgiref",
    "xml",
    "xmlrpc",
    "zipapp",
    "zipfile",
    "zipimport",
    "zlib",
    "zoneinfo",
];

/// A file that decides the verdict rather than implements the change.
fn harness_file(path: &str) -> bool {
    if HARNESS_FILES.iter().any(|g| glob_match(g, path)) {
        return true;
    }
    // Only at the top of the checkout, where `python -m` and pytest put it
    // first on sys.path; `pkg/types.py` shadows nothing.
    let module = match path.split('/').collect::<Vec<_>>().as_slice() {
        [file] => file.strip_suffix(".py").unwrap_or(""),
        [dir, "__init__.py"] => dir,
        _ => return false,
    };
    PYTHON_STDLIB.contains(&module)
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

/// Marks every tracked file of a checkout read-only (directories stay
/// writable: builds create their outputs there).
fn read_only_tracked(dir: &Path) {
    let Ok(o) = Command::new("git")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .args(["ls-files", "-z"])
        .current_dir(dir)
        .output()
    else {
        return;
    };
    for f in o.stdout.split(|b| *b == 0).filter(|f| !f.is_empty()) {
        let path = dir.join(String::from_utf8_lossy(f).as_ref());
        if let Ok(m) = std::fs::symlink_metadata(&path)
            && m.is_file()
        {
            let mut perm = m.permissions();
            perm.set_readonly(true);
            let _ = std::fs::set_permissions(&path, perm);
        }
    }
}

/// Whether other processes loaded the machine much more during one arm's
/// trials than the other's: (CPU ms used by others, trial wall ms) per
/// trial. Interleaving spreads background noise evenly; a large difference
/// is aimed at one arm (round 9: a burner started through systemd-run).
pub fn load_imbalance(base: &[(f64, f64)], cand: &[(f64, f64)]) -> Option<String> {
    fn median(mut x: Vec<f64>) -> f64 {
        x.sort_by(f64::total_cmp);
        x.get(x.len() / 2).copied().unwrap_or(0.0)
    }
    // Cores busy with other work: a longer trial collects more of the same
    // background load, so compare rates.
    let rate = |v: &[(f64, f64)]| {
        median(
            v.iter()
                .filter(|(_, w)| *w > 0.0)
                .map(|(ms, w)| ms / w)
                .collect(),
        )
    };
    let (b, c) = (rate(base), rate(cand));
    // CPU time is counted in 10 ms ticks: under a second per trial the rate
    // is noise, so only longer trials are judged.
    let wall = median(base.iter().chain(cand).map(|(_, w)| *w).collect());
    if wall < 1000.0 || (b - c).abs() <= 0.5f64.max(0.5 * b.max(c)) {
        return None;
    }
    // Aimed load is on every trial of one arm; a busy machine's load comes
    // and goes (round 10: parallel docker builds voided an honest run). Only
    // a complete separation counts: every trial of the busier arm saw more
    // than every trial of the other (under 1% by chance with 5 + 5).
    let rates = |v: &[(f64, f64)]| -> Vec<f64> {
        v.iter()
            .filter(|(_, w)| *w > 0.0)
            .map(|(ms, w)| ms / w)
            .collect()
    };
    let (rb, rc) = (rates(base), rates(cand));
    let (hi, lo) = if b > c { (&rb, &rc) } else { (&rc, &rb) };
    let separated = hi.iter().cloned().fold(f64::INFINITY, f64::min)
        > lo.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    if !separated {
        return None;
    }
    let busier = if b > c { "baseline" } else { "candidate" };
    Some(format!(
        "the machine was busier during the {busier}'s trials: other processes kept {:.1} cores busy there, {:.1} during the other arm's; something outside the trials disturbed one arm, so the comparison is void (rerun on a quiet machine)",
        b.max(c),
        b.min(c)
    ))
}

/// Whether trials can run in a bubblewrap sandbox here (ADR 0024).
pub fn sandbox_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        cfg!(target_os = "linux")
            && Command::new("bwrap")
                .args([
                    "--ro-bind",
                    "/",
                    "/",
                    "--dev",
                    "/dev",
                    "--proc",
                    "/proc",
                    "--unshare-pid",
                    "true",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
    })
}

/// A command for one arm, sandboxed when bubblewrap works (ADR 0024): the
/// whole filesystem read-only except the arm's own checkout, its private
/// state (caches, bytecode), a private /tmp and the toolchain caches builds
/// must write; its own process namespace, which dies with prova. The other
/// arm's checkout, prova's store and the user's files cannot be changed.
fn sandboxed(dir: &Path, program: &str, args: &[String], network: bool) -> Command {
    let state = pycache_dir(dir);
    let _ = std::fs::create_dir_all(&state);
    let mut c = if sandbox_available() {
        let mut c = Command::new("bwrap");
        c.args([
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--tmpfs",
            "/tmp",
        ]);
        c.arg("--bind").arg(dir).arg(dir);
        c.arg("--bind").arg(&state).arg(&state);
        // Shared toolchain caches stay read-only (round 11: a candidate
        // patched the baseline's compiled test in the shared Go build
        // cache); each arm writes its own caches, set below.
        c.args(["--unshare-pid", "--die-with-parent", "--new-session"]);
        if !network {
            // Loopback only: no accomplice on the network or the host.
            c.arg("--unshare-net");
        }
        c.arg("--").arg(program).args(args);
        c.env("TMPDIR", "/tmp");
        c
    } else {
        let mut c = Command::new(program);
        c.args(args);
        c
    };
    // Per-arm caches, sandboxed or not: nothing one arm compiles is read
    // by the other.
    c.env("GOCACHE", state.join("go-build"))
        .env("XDG_CACHE_HOME", state.join("cache"))
        .env("npm_config_cache", state.join("npm"));
    // The session's secrets are none of the workload's business (round 11).
    for (k, _) in std::env::vars() {
        let u = k.to_ascii_uppercase();
        if [
            "TOKEN",
            "SECRET",
            "PASSWORD",
            "PASSWD",
            "API_KEY",
            "CREDENTIAL",
            "COOKIE",
        ]
        .iter()
        .any(|w| u.contains(w))
        {
            c.env_remove(&k);
        }
    }
    c
}

/// A private bytecode cache per checkout, outside both: Python reads
/// `.pyc` files only from there, so one planted in a checkout's ignored
/// `__pycache__` is never used (round 10).
fn pycache_dir(dir: &Path) -> PathBuf {
    let key = sha256_hex(dir.to_string_lossy().as_bytes());
    // Outside /tmp: the sandbox gives each command a private /tmp.
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    base.join(format!("prova-arm-{}", &key[..16]))
}

fn base_name_for(store: &Store, comparison: &str) -> Result<String> {
    Ok(store.conn().query_row(
        "SELECT baseline FROM comparisons WHERE id = ?1",
        params![comparison],
        |r| r.get(0),
    )?)
}

/// A variant whose median in this comparison is 2x away from its median in
/// earlier decided comparisons of the experiment.
fn drifted(
    store: &Store,
    exp: &str,
    comparison: &str,
    metric: &str,
    arms: [&Arm; 2],
    names: [&str; 2],
) -> Result<Option<String>> {
    let (now_b, now_c) = samples(store, comparison, metric)?;
    for (i, now) in [now_b, now_c].into_iter().enumerate() {
        let mut stmt = store.conn().prepare(
            "SELECT s.value FROM samples s
               JOIN trials t ON t.comparison = s.comparison AND t.seq = s.seq
               JOIN comparisons c ON c.id = s.comparison
              WHERE c.experiment = ?1 AND c.id <> ?2 AND c.status = 'decided' AND s.metric = ?3
                AND t.warmup = 0 AND t.state = 'complete'
                AND ((t.arm = 'baseline' AND c.baseline = ?4) OR (t.arm = 'candidate' AND c.candidate = ?4))",
        )?;
        let before: Vec<f64> = stmt
            .query_map(params![exp, comparison, metric, names[i]], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if before.len() < 5 || now.is_empty() {
            continue;
        }
        let (m0, m1) = (stats::median(&before), stats::median(&now));
        if m0 > 0.0 && m1 > 0.0 && (m1 / m0).max(m0 / m1) >= 2.0 {
            return Ok(Some(format!(
                "the {} ({}) measured {} now but {} in this experiment's earlier comparisons ({:.1}x): something disturbed it or its workload changed, so the comparison is void",
                arms[i].as_str(),
                names[i],
                short(m1),
                short(m0),
                (m1 / m0).max(m0 / m1)
            )));
        }
    }
    Ok(None)
}

/// Whether one arm's trials sat stopped (SIGSTOP from outside, or starved)
/// while the other's ran: (own CPU ms, wall ms) per trial. A CPU-bound
/// workload keeps about one core busy; one arm far below it, every trial,
/// while the other is near it, was held back (round 11).
pub fn stopped_arm(base: &[(f64, f64)], cand: &[(f64, f64)]) -> Option<String> {
    let util = |v: &[(f64, f64)]| -> Vec<f64> {
        v.iter()
            .filter(|(_, w)| *w >= 1000.0)
            .map(|(ms, w)| ms / w)
            .collect()
    };
    let (ub, uc) = (util(base), util(cand));
    if ub.len() < 3 || uc.len() < 3 {
        return None;
    }
    let max = |v: &[f64]| v.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let min = |v: &[f64]| v.iter().cloned().fold(f64::INFINITY, f64::min);
    let (low, high, arm) = if max(&ub) < min(&uc) {
        (&ub, &uc, "baseline")
    } else if max(&uc) < min(&ub) {
        (&uc, &ub, "candidate")
    } else {
        return None;
    };
    // Held back: every trial of one arm under 60% of a core while every
    // trial of the other kept at least 90% of one busy.
    (max(low) < 0.6 && min(high) >= 0.9).then(|| {
        format!(
            "the {arm}'s trials ran only {:.0}% of their time while the other arm's ran {:.0}% or more: something stopped or starved that arm (SIGSTOP from outside?), so the comparison is void",
            max(low) * 100.0,
            min(high) * 100.0
        )
    })
}

/// The first tracked file whose bytes differ from the blob the commit
/// records, hashed without any filter.
fn checkout_differs(dir: &Path) -> Option<String> {
    let listed = Command::new("git")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .args(["ls-files", "-s", "-z"])
        .current_dir(dir)
        .output()
        .ok()?;
    let entries: Vec<(String, String)> = listed
        .stdout
        .split(|b| *b == 0)
        .filter(|e| !e.is_empty())
        .filter_map(|e| {
            let e = String::from_utf8_lossy(e);
            let (meta, path) = e.split_once('\t')?;
            let mut f = meta.split_whitespace();
            let mode = f.next()?;
            let blob = f.next()?;
            // Regular files only (not symlinks or submodules).
            mode.starts_with("100")
                .then(|| (blob.to_owned(), path.to_owned()))
        })
        .collect();
    if entries.is_empty() {
        return None;
    }
    let mut child = Command::new("git")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .args(["hash-object", "--no-filters", "--stdin-paths"])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let paths: String = entries.iter().map(|(_, p)| format!("{p}\n")).collect();
    let mut stdin = child.stdin.take()?;
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(paths.as_bytes());
    });
    let out = child.wait_with_output().ok()?;
    let _ = writer.join();
    let hashes = String::from_utf8_lossy(&out.stdout).into_owned();
    entries
        .iter()
        .zip(hashes.lines())
        .find(|((blob, _), h)| blob != h)
        .map(|((_, p), _)| p.clone())
}

/// `git status` of tracked files in a checkout (empty outside git).
fn tracked_state(dir: &Path) -> String {
    Command::new("git")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
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
    let status = sandboxed(
        dir,
        "sh",
        &["-c".to_owned(), oracle.clone()],
        p.allow_network,
    )
    .env_remove("PROVA_HOME")
    .env("PYTHONNOUSERSITE", "1")
    .env("PYTHONPYCACHEPREFIX", pycache_dir(dir))
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
        return Ok(Err(SpecFail::Violated(format!(
            "the oracle left process {l} running after it ended; it would disturb the trials"
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
    // The checkouts' tracked files are read-only: an oracle that writes
    // one (go generate) fails for that, not for the spec (round 10).
    let text = std::fs::read_to_string(logs.join(&name)).unwrap_or_default();
    let hint = if text.contains("ermission denied") || text.contains("Read-only file system") {
        "; its log shows a write was refused: during experiments the checkouts' tracked files are read-only, so an oracle must not rewrite them (generate into a temporary directory instead)"
    } else {
        ""
    };
    // The decisive line, so the reader need not open the log.
    let last = decisive_line(&text)
        .map(|l| l.chars().take(200).collect::<String>())
        .unwrap_or_else(|| format!("oracle `{oracle}` failed, printing nothing"));
    Ok(Err(SpecFail::Violated(format!(
        "{last} (full log: prova exp log <experiment> {arm_log}){hint}"
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
    // Holm across this experiment's decided comparisons plus this one.
    let mut stmt = store.conn().prepare(
        "SELECT json_extract(result, '$.primary.p') FROM comparisons WHERE experiment = ?1 AND status = 'decided' AND id <> ?2",
    )?;
    let mut ps: Vec<f64> = stmt
        .query_map(params![e.name, comparison], |r| r.get::<_, Option<f64>>(0))?
        .filter_map(|r| r.ok().flatten())
        .collect();
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
    Ok((
        verdict,
        serde_json::json!({ "primary": primary, "guardrails": guards, "reason": reason, "comparisons_in_family": ps.len() }),
    ))
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
        && !Command::new("git").env("GIT_NO_REPLACE_OBJECTS", "1")
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
    let tx = store.conn().unchecked_transaction()?;
    tx.execute(
        "UPDATE comparisons SET status = 'decided', verdict = ?2, result = ?3, ended_ms = ?4 WHERE id = ?1",
        params![comparison, verdict.as_str(), result.to_string(), now_ms() as i64],
    )?;
    let decision = if verdict == Verdict::Accept {
        "keep"
    } else {
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
    /// Stored verdicts or a best that the immutable samples do not support
    /// (an edited database): each is a reason not to trust the record.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub integrity: Vec<String>,
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
    let e = get(store, exp)?;
    let comparison = match latest_comparison(store, exp)? {
        Some(id) => Some(comparison_report(store, &e, &id)?),
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
        .map(|_| format!("prova exp log {exp}"));
    let integrity = integrity(store, &e)?;
    Ok(Report {
        experiment: e.name,
        hypothesis: e.hypothesis,
        protocol_hash: e.protocol_hash,
        protocol: e.protocol,
        best: e.best,
        comparison,
        decisions,
        logs,
        integrity,
    })
}

/// Re-derives every stored ACCEPT from the immutable samples, with the Holm
/// family as it stood when it was decided, and checks the best against the
/// record (round 11: editing prova.db turned a REJECT into an ACCEPT). The
/// same user can edit the file; this makes it show.
fn integrity(store: &Store, e: &Experiment) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut stmt = store.conn().prepare(
        "SELECT id, candidate, COALESCE(ended_ms, 0) FROM comparisons
          WHERE experiment = ?1 AND status = 'decided' AND verdict = 'accept'",
    )?;
    let accepted: Vec<(String, String, i64)> = stmt
        .query_map(params![e.name], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let p = &e.protocol;
    for (id, cand, ended) in &accepted {
        let (base, c) = samples(store, id, &p.metric)?;
        if base.len() < p.min_trials as usize || c.len() < p.min_trials as usize {
            out.push(format!(
                "comparison {id} is recorded as ACCEPT but has no measurements to support it"
            ));
            continue;
        }
        let mut fam = store.conn().prepare(
            "SELECT json_extract(result, '$.primary.p') FROM comparisons
              WHERE experiment = ?1 AND status = 'decided' AND id <> ?2 AND COALESCE(ended_ms, 0) < ?3",
        )?;
        let mut ps: Vec<f64> = fam
            .query_map(params![e.name, id, ended], |r| r.get::<_, Option<f64>>(0))?
            .filter_map(|r| r.ok().flatten())
            .collect();
        let raw = stats::mann_whitney(&base, &c).p;
        ps.push(raw);
        let adjusted = *stats::holm(&ps).last().unwrap_or(&raw);
        let primary = measure(&p.metric, p.better, &base, &c, p.alpha, Some(adjusted));
        let (v, _) = judge(&primary, &[], p.min_effect, p.alpha);
        if v != Verdict::Accept {
            out.push(format!(
                "comparison {id} ({cand}) is recorded as ACCEPT, but its own samples give {}: the record was changed",
                v.as_str()
            ));
        }
    }
    let best_ok = e.best == "baseline"
        || accepted.iter().any(|(_, c, _)| *c == e.best)
        || store
            .conn()
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM best_resets WHERE experiment = ?1 AND to_best = ?2)",
                params![e.name, e.best],
                |r| r.get::<_, bool>(0),
            )
            .unwrap_or(false);
    if !best_ok {
        out.push(format!(
            "the best is {}, but no ACCEPT or recorded reset made it so: the record was changed",
            e.best
        ));
    }
    Ok(out)
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
mod stopped_tests {
    #[test]
    fn an_arm_held_back_while_the_other_ran_is_caught() {
        let ran = vec![(1950.0, 2000.0); 5];
        let held = vec![(1000.0, 2000.0); 5];
        assert!(super::stopped_arm(&held, &ran).is_some());
        // A parallel speedup: more than a core is fine, the baseline ran.
        let parallel = vec![(7000.0, 2000.0); 5];
        assert!(super::stopped_arm(&ran, &parallel).is_none());
        // Both mostly waiting (sleep-bound workloads): nothing to say.
        assert!(super::stopped_arm(&[(100.0, 2000.0); 5], &[(200.0, 2000.0); 5]).is_none());
    }
}

#[cfg(test)]
mod load_tests {
    #[test]
    fn a_burner_on_one_arm_voids_but_even_noise_does_not() {
        let quiet = vec![(100.0, 2000.0); 5];
        let burned = vec![(8000.0, 2000.0); 5];
        assert!(super::load_imbalance(&quiet, &burned).is_some());
        let busy = vec![(6000.0, 2000.0); 5];
        let busy2 = vec![(5000.0, 1800.0); 5];
        assert!(super::load_imbalance(&busy, &busy2).is_none());
        // Load that comes and goes, even if one arm's median is higher.
        let a = [
            (100.0, 2000.0),
            (9000.0, 2000.0),
            (8000.0, 2000.0),
            (200.0, 2000.0),
            (7000.0, 2000.0),
        ];
        let b = [
            (150.0, 2000.0),
            (300.0, 2000.0),
            (8500.0, 2000.0),
            (100.0, 2000.0),
            (250.0, 2000.0),
        ];
        assert!(super::load_imbalance(&a, &b).is_none());
        // Short trials: too coarse to judge.
        assert!(super::load_imbalance(&[(10.0, 20.0); 5], &[(60.0, 20.0); 5]).is_none());
    }
}

#[cfg(test)]
mod tests {
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
