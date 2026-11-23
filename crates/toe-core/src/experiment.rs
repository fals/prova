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
        Some(if self.base <= self.cand {
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
    let (a, b) = (sign * hl.lower / scale, sign * hl.upper / scale);
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
    let pct = |v: f64| format!("{:+.1}%", v * 100.0);
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
    let ci = format!("[{}, {}]", pct(lo), pct(hi));
    // The change as a ratio of medians ("37.6x"), easier to read than +97%.
    let (b, c) = (primary.baseline_median, primary.candidate_median);
    let ratio = if b > 0.0 && c > 0.0 && b.is_finite() && c.is_finite() {
        format!(
            ", median {} -> {} ({:.2}x)",
            short(b),
            short(c),
            if b > c { b / c } else { c / b }
        )
    } else {
        String::new()
    };
    // With few trials the exact rank test cannot go lower: say so.
    let floor = crate::stats::min_two_sided_p(primary.n_baseline, primary.n_candidate);
    let at_floor = if primary.p_adjusted <= floor * (1.0 + 1e-9) {
        format!(
            "; {:.4} is the smallest p possible with {} + {} trials",
            floor, primary.n_baseline, primary.n_candidate
        )
    } else {
        String::new()
    };
    if primary.p_adjusted < alpha && lo > min_effect {
        (
            Verdict::Accept,
            format!(
                "{} improved {}{ratio} (CI {ci}, p={:.4}{at_floor})",
                primary.metric,
                pct(primary.improvement),
                primary.p_adjusted
            ),
        )
    } else if primary.p_adjusted < alpha && hi < 0.0 {
        (
            Verdict::Reject,
            format!(
                "{} got worse by {}{ratio} (CI {ci}, p={:.4}{at_floor})",
                primary.metric,
                pct(-primary.improvement).trim_start_matches('+').to_owned(),
                primary.p_adjusted
            ),
        )
    } else if lo > -min_effect && hi < min_effect {
        (
            Verdict::Reject,
            format!(
                "no practical effect: CI {ci} inside ±{}",
                pct(min_effect).trim_start_matches('+')
            ),
        )
    } else {
        (
            Verdict::Inconclusive,
            format!(
                "CI {ci} straddles the ±{} threshold (p={:.4})",
                pct(min_effect).trim_start_matches('+'),
                primary.p_adjusted
            ),
        )
    }
}

/// A number with about 4 significant digits, for messages.
fn short(v: f64) -> String {
    if v.abs() >= 1000.0 {
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
            "no experiment named {name:?}; create it with `toe exp new`"
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
/// file (tracked and untracked), `.toe/` excluded. The commit is kept alive
/// by `refs/toe/candidates/<exp>/<label>` so `git gc` cannot drop it. `None`
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
            .env("GIT_AUTHOR_NAME", "toe")
            .env("GIT_AUTHOR_EMAIL", "toe@localhost")
            .env("GIT_COMMITTER_NAME", "toe")
            .env("GIT_COMMITTER_EMAIL", "toe@localhost")
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
        let mut add = vec!["add", "-A", "--", ".", ":(exclude,glob)**/.toe/**"];
        // A store kept inside the repository is toe's own, never a candidate.
        let inside = store
            .dir()
            .strip_prefix(&top)
            .ok()
            .map(|p| format!(":(exclude){}", p.display()));
        if let Some(x) = &inside {
            add.push(x);
        }
        run(&add)?;
        let tree = run(&["write-tree"])?;
        let msg = format!("toe candidate {exp}/{label}");
        let mut ct = vec!["commit-tree", &tree, "-m", &msg];
        if let Some(h) = &head {
            ct.extend(["-p", h]);
        }
        let sha = run(&ct)?;
        run(&[
            "update-ref",
            &format!("refs/toe/candidates/{exp}/{label}"),
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
            "protocol frozen at first trial (hash {}); start a new experiment: toe exp new {name}-2 ...",
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
            "comparison {id} is still running; toe exp wait {exp}"
        ));
    }
    let id = new_run_id();
    let tx = store.conn().unchecked_transaction()?;
    tx.execute(
        "UPDATE experiments SET frozen = 1 WHERE name = ?1",
        params![exp],
    )?;
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
/// benchmark recognizers live in toe-adapters.
pub type MetricParser = dyn Fn(&str) -> Vec<(String, f64, Option<String>)>;

fn run_trial(
    dir: &Path,
    p: &Protocol,
    v: &Variant,
    log: &Path,
    parse: &MetricParser,
) -> Result<TrialResult> {
    let mut log = private_file(log)?;
    let mut cmd = Command::new(&p.command[0]);
    cmd.args(&p.command[1..])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    for (k, val) in &v.params {
        cmd.env(format!("TOE_PARAM_{}", k.to_ascii_uppercase()), val);
    }
    let start = Instant::now();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            writeln!(log, "toe: could not start workload: {e}")?;
            return Ok(TrialResult {
                exit_code: None,
                wall_ms: 0.0,
                metrics: vec![],
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
            writeln!(log, "toe: killed after {}ms", p.kill_after_ms)?;
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
    process::kill_group(pgid); // leftovers must not disturb the next trial
    if let Ok(e) = err_thread.join() {
        log.write_all(&e)?;
        text.push_str(&String::from_utf8_lossy(&e));
    }
    let metrics = parse(&text);
    Ok(TrialResult {
        exit_code: status.and_then(|s| s.code()),
        wall_ms,
        metrics,
    })
}

fn fingerprint() -> String {
    sha256_hex(machine_fingerprint().to_string().as_bytes())[..12].to_owned()
}

// ------------------------------------------------------------ worker

/// Body of `toe __exp`: runs trials and decides.
pub fn work(store: &Store, comparison: &str, parse: &MetricParser) -> Result<()> {
    let result = work_comparison(store, comparison, parse);
    let cleaned = keep_failed_logs(store, comparison);
    result.and(cleaned)
}

/// After a comparison (ADR 0016): the logs of what failed (a trial, the
/// setup, the oracle) are kept as excerpts in the store; the log directory
/// and the baseline worktrees are removed.
fn keep_failed_logs(store: &Store, comparison: &str) -> Result<()> {
    let logs = store.dir().join("experiments").join(comparison);
    let (exp, base, cand, status, result): (String, String, String, String, Option<String>) =
        store.conn().query_row(
            "SELECT experiment, baseline, candidate, status, result FROM comparisons WHERE id = ?1",
            params![comparison],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?;
    let failed_trials: Vec<u32> = {
        let mut stmt = store
            .conn()
            .prepare("SELECT seq FROM trials WHERE comparison = ?1 AND state = 'failed'")?;
        stmt.query_map(params![comparison], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    let violated = result
        .as_deref()
        .is_some_and(|r| r.contains("\"specification\":\"violated\""));
    let mut keep: Vec<String> = failed_trials.iter().map(|s| s.to_string()).collect();
    for arm in ["baseline", "candidate"] {
        if status == "failed" {
            keep.push(format!("setup-{arm}"));
        }
        if status == "failed" || violated {
            keep.push(format!("oracle-{arm}"));
        }
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

fn work_comparison(store: &Store, comparison: &str, parse: &MetricParser) -> Result<()> {
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
        )?;
        let ok = r.exit_code == Some(0);
        let tx = store.conn().unchecked_transaction()?;
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
            let ok = Command::new("sh")
                .arg("-c")
                .arg(setup)
                .current_dir(&dirs[i])
                .stdin(Stdio::null())
                .stdout(log.try_clone()?)
                .stderr(log)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                let reason = format!(
                    "setup `{setup}` failed for {} (`toe exp log {exp} setup-{}`)",
                    arm.as_str(),
                    arm.as_str()
                );
                return end_failed(store, &exp, comparison, &cand_name, &reason);
            }
        }
    }
    let mut c = Comparison::new(p.min_trials, p.max_trials);
    for (i, (arm, _)) in arms.iter().enumerate() {
        let verdict = satisfies_spec(p, &dirs[i], &logs, *arm)?;
        c.check_spec(*arm, verdict.is_ok());
        if let Err(why) = verdict {
            let why = why.replace("<experiment>", &exp);
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
    for _ in 0..p.warmup {
        for arm in [Arm::Baseline, Arm::Candidate] {
            if !run_one(arm, true)? {
                return end_failed(
                    store,
                    &exp,
                    comparison,
                    &cand_name,
                    &format!("{} warmup trial failed", arm.as_str()),
                );
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
                let reason = format!(
                    "{} trial {} failed (exit != 0; `toe exp log {exp} {}`)",
                    arm.as_str(),
                    counter.get(),
                    counter.get()
                );
                return end_failed(store, &exp, comparison, &cand_name, &reason);
            }
            c.trial(&fingerprint_of(store, comparison, counter.get())?);
        }
        let (outcome, result) = evaluate(store, &e, comparison)?;
        if outcome == Verdict::Inconclusive && target < p.max_trials {
            target = (target + 2).min(p.max_trials);
            c.verdict = Some(Verdict::Inconclusive);
            continue;
        }
        let verdict = c.decide(outcome).unwrap_or(Verdict::Invalid);
        let mut result = result;
        // Say that the specification was checked, not only when it failed
        // (ADR 0018): both arms satisfied it before any trial ran.
        if let Some(oracle) = &p.oracle {
            result["specification"] =
                serde_json::json!(format!("holds on both arms (oracle `{oracle}`)"));
        } else if !p.specs.is_empty() {
            result["specification"] =
                serde_json::json!("specification files unchanged on both arms (no oracle)");
        }
        return finish(store, &e, comparison, &cand_name, verdict, result);
    }
}

/// Whether a variant satisfies the protocol's specification: its spec files
/// are the registered ones, and the oracle succeeds in its directory.
fn satisfies_spec(
    p: &Protocol,
    dir: &Path,
    logs: &Path,
    arm: Arm,
) -> Result<std::result::Result<(), String>> {
    for f in &p.specs {
        let found = std::fs::read(dir.join(&f.path)).map(|b| sha256_hex(&b));
        match found {
            Ok(h) if h == f.sha256 => {}
            Ok(_) => {
                return Ok(Err(format!(
                    "{} differs from the registered specification",
                    f.path
                )));
            }
            Err(_) => {
                return Ok(Err(format!(
                    "{} is missing (commit the specification before registering it)",
                    f.path
                )));
            }
        }
    }
    let Some(oracle) = &p.oracle else {
        return Ok(Ok(()));
    };
    let name = format!("oracle-{}.log", arm.as_str());
    let log = private_file(&logs.join(&name))?;
    let ok = Command::new("sh")
        .arg("-c")
        .arg(oracle)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    Ok(if ok {
        Ok(())
    } else {
        Err(format!(
            "oracle `{oracle}` failed (`toe exp log <experiment> {}`)",
            name.trim_end_matches(".log")
        ))
    })
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
    if verdict == Verdict::Accept {
        tx.execute(
            "UPDATE experiments SET best = ?2 WHERE name = ?1",
            params![e.name, candidate],
        )?;
    }
    tx.commit()?;
    Ok(())
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
        "SELECT comparison, candidate, decision, reason FROM decisions WHERE experiment = ?1 ORDER BY created_ms",
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
        .map(|_| format!("toe exp log {exp}"));
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
        assert_eq!(
            arms,
            vec![Arm::Baseline, Arm::Candidate, Arm::Baseline, Arm::Candidate]
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
